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
use crate::ipc_error::CommandError;
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
) -> Result<WorktreeFamily, CommandError> {
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
pub fn admit_worktree_pair(
    repo_path: &str,
    worktree_path: &str,
) -> Result<AdmittedPair, CommandError> {
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
        )
        .into());
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
        self.scan_checkouts(true)
    }

    /// [`Self::metadata_checkouts`] without the admitted pair: the primary
    /// working tree first, then each linked worktree the registry names. What
    /// the metadata says is registered, whatever the caller asked about.
    fn registered_checkouts(&self) -> Option<Vec<PathBuf>> {
        self.scan_checkouts(false)
    }

    fn scan_checkouts(&self, with_pair: bool) -> Option<Vec<PathBuf>> {
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
        let mut checkouts = vec![primary];
        if with_pair {
            checkouts.push(self.repo.clone());
            checkouts.push(self.worktree.clone());
        }

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
    pub fn authenticate(self) -> Result<WorktreeFamily, CommandError> {
        // The registered checkouts, never including the pair asked about: a
        // worktree whose registration is gone must change the fingerprint, not
        // be written into it. The first is the primary working tree.
        let registered = self.registered_checkouts();
        let registry = registered.as_ref().map(|checkouts| {
            let mut sorted = checkouts.clone();
            sorted.sort();
            sorted.dedup();
            sorted
        });
        if let (Some(registered), Some(registry)) = (&registered, &registry) {
            if let Some(family) =
                remembered_family(&self.common, registry, &self.repo, &self.worktree)
            {
                return Ok(family);
            }
            // A fresh process has no memory of its own; an earlier one may have
            // left Git's answer for this very registry (`worktree_family_disk`).
            if let Some(family) = self.family_from_disk(registered, registry) {
                remember_family(self.common.clone(), registry.clone(), &family);
                return Ok(family);
            }
        }
        let common = self.common.clone();
        let family = self.authenticate_with_git()?;
        if let Some(registry) = registry {
            super::worktree_family_disk::store(&common, &registry, &family.members);
            remember_family(common, registry, &family);
        }
        Ok(family)
    }

    /// The family for this pair from what an earlier process stored, with
    /// nothing in the file trusted as a source of membership. The stored list
    /// is only Git's *order*: it is used when, as a set, it equals the registry
    /// the metadata names right now, and both paths of the pair are in that
    /// registry. The anchor is the primary working tree the metadata proved to
    /// hold this common directory (`registered[0]`): the checkout
    /// [`Self::authenticate_with_git`] takes from Git's first entry and then
    /// checks belongs to this repository, which is how [`Self::scan_checkouts`]
    /// found it. Anything else is a miss.
    fn family_from_disk(
        &self,
        registered: &[PathBuf],
        registry: &[PathBuf],
    ) -> Option<WorktreeFamily> {
        let anchor = registered.first()?.clone();
        if !registry.contains(&self.repo) || !registry.contains(&self.worktree) {
            return None;
        }
        let stored = super::worktree_family_disk::load(&self.common)?;
        let mut as_set = stored.members.clone();
        as_set.sort();
        as_set.dedup();
        if stored.registry != registry || as_set != registry {
            return None;
        }
        let mut members = vec![anchor.clone()];
        members.extend(stored.members.into_iter().filter(|path| path != &anchor));
        Some(WorktreeFamily {
            anchor,
            worktree: self.worktree.clone(),
            members,
        })
    }

    fn authenticate_with_git(self) -> Result<WorktreeFamily, CommandError> {
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
            return Err("Git returned no active worktrees for this repository"
                .to_string()
                .into());
        };
        if !registered.iter().any(|path| path == &repo) {
            return Err(format!(
                "Repository '{}' is not a registered worktree",
                repo.display()
            )
            .into());
        }
        if !registered.iter().any(|path| path == &worktree) {
            return Err(format!(
                "Worktree '{}' is not registered with repository '{}'",
                worktree.display(),
                repo.display()
            )
            .into());
        }

        // The pair shares one common directory (admission checked it), so only a
        // third checkout as the anchor needs its own read.
        let anchor_common = if anchor == repo || anchor == worktree {
            repo_common.clone()
        } else {
            resolve_git_common_dir(&anchor)?
        };
        if anchor_common != repo_common {
            return Err("Git's primary worktree belongs to a different repository"
                .to_string()
                .into());
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

/// Makes the next call in this process ask the disk, as a new process would.
#[cfg(all(test, unix))]
pub(crate) fn forget_in_process(common: &Path) {
    remembered_families()
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(
            common.to_path_buf(),
            Remembered {
                registry: Vec::new(),
                members: Vec::new(),
            },
        );
}

/// Remembers families on disk, under test, in a directory on this thread.
#[cfg(all(test, unix))]
pub(crate) use super::worktree_family_disk::test_dir as family_disk_dir;

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

use crate::engine::cow_clone::clone_ignored_caches;
use crate::engine::portless::{detect_worktree_routes, PortlessSnapshot, WorktreeRouteInfo};
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
    Failed(CommandError),
}

/// One point in time past which a scan stops starting work and stops waiting
/// on what it started. Handed down through every stage of a pass, so each
/// stage asks the same clock rather than being given its own slice of it.
#[derive(Clone, Copy, Debug)]
pub struct ScanDeadline {
    /// When the pass the deadline belongs to began: the moment its caller
    /// asked, which is what a status read shared inside the pass is measured
    /// against ([`status_cell`]).
    start: Instant,
    end: Instant,
}

impl ScanDeadline {
    /// The deadline `budget` after `start`.
    pub fn after(start: Instant, budget: Duration) -> Self {
        Self {
            start,
            end: start.checked_add(budget).unwrap_or(start),
        }
    }

    pub fn expired(&self) -> bool {
        Instant::now() >= self.end
    }

    pub fn remaining(&self) -> Duration {
        self.end.saturating_duration_since(Instant::now())
    }

    /// When the pass began.
    pub fn started(&self) -> Instant {
        self.start
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
pub(crate) fn read_worktree_entries(repo: &Path) -> Result<Vec<ParsedWorktree>, CommandError> {
    let run =
        crate::engine::git_cli::git_captured(repo, &["worktree", "list", "--porcelain", "-z"])
            .and_then(|run| run.require_complete("Worktree listing"))?;
    if !run.success {
        return Err(format!(
            "Could not list this repository's worktrees: {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )
        .into());
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
pub fn refuse_gitpulse_lane_target(target_path: &str) -> Result<(), CommandError> {
    match agent_layout(target_path) {
        Some(layout) if is_gitpulse_lane(&layout.kind) => Err(format!(
            "{target_path} is inside {GITPULSE_LANE_DIR}/, where GitPulse places the worktrees it creates for task attempts. Start the task from the task board to get one, or choose a path outside {GITPULSE_LANE_DIR}/."
        ).into()),
        _ => Ok(()),
    }
}

/// Ceiling on paths returned by [`changed_paths`]. Collision detection only
/// needs identity, and a worktree that dirtied tens of thousands of files
/// must not turn one insights call into an unbounded allocation.
pub const MAX_CHANGED_PATHS: usize = 256;

/// The spawn behind every `git status --porcelain -z` read of a worktree.
/// An unfiltered read is made only by [`status_cell`], which is the one owner
/// of "what status says about this worktree"; the pathspec-limited read
/// ([`changed_paths_only`]) is a different question with a different answer
/// and spawns here directly.
///
/// With a `deadline`, a read that has not started by then does not start, and
/// one in flight is given the time left (at least [`STATUS_GRACE`]) rather
/// than git's 90 s command timeout. Either way the caller gets an error that
/// says why, which is how a worktree it could not read stays "unknown".
///
/// `only` limits the read to some paths (`:(literal)` pathspecs, so `*?[` in a
/// name never widen them; `icase` too when the volume ignores case): the
/// answer to "is this file dirty here" without walking the rest of the tree's
/// changes.
fn spawn_porcelain(
    dir: &Path,
    deadline: Option<&ScanDeadline>,
    only: Option<OnlyPaths<'_>>,
) -> Result<String, CommandError> {
    #[cfg(test)]
    {
        status_probes::fire(dir);
        // Stands in for a hung git ending at its own time limit.
        if only.is_none() && status_probes::forced_timeout(dir) {
            return Err("git status timed out after 0.3s".to_string().into());
        }
    }
    let magic = match only {
        Some(only) if only.ignore_case => ":(literal,icase)",
        _ => ":(literal)",
    };
    let specs: Vec<String> = only
        .iter()
        .flat_map(|only| only.paths.iter())
        .map(|path| format!("{magic}{path}"))
        .collect();
    let mut args: Vec<&str> = crate::engine::git_cli::STATUS_ARGV.to_vec();
    if !specs.is_empty() {
        args.push("--");
        args.extend(specs.iter().map(String::as_str));
    }
    match deadline {
        // The unfiltered read is the one an identical caller may share while
        // it is still queued at the spawn gate.
        None if only.is_none() => crate::engine::git_cli::git_text_shared(dir, &args),
        None => git_text(dir, &args),
        Some(deadline) if deadline.expired() => Err(DEADLINE_PASSED.to_string().into()),
        Some(deadline) => git_text_with_timeout(dir, &args, deadline.remaining().max(STATUS_GRACE)),
    }
}

/// The status of `dir` as of some moment no earlier than `not_before`, from
/// the worktree's [`status_cell`]: shared with every other reader of the same
/// worktree in the same admission class, and spawned at most once for all of
/// them.
pub(crate) fn read_status_since(
    dir: &Path,
    deadline: Option<&ScanDeadline>,
    not_before: Instant,
) -> Result<PorcelainRead, CommandError> {
    status_cell::read(dir, deadline, not_before)
}

pub(crate) use status_cell::PorcelainRead;

/// How long ago `dir`'s status was last read and kept in the cell it is shared
/// through, in the caller's admission class. `None` when no read is kept.
pub(crate) fn status_age(dir: &Path) -> Option<Duration> {
    status_cell::age(dir)
}

/// Cells kept at once, across every worktree of every repository.
#[cfg(test)]
pub(crate) const STATUS_CELL_CAP: usize = status_cell::MAX_CELLS;

/// The one owner of "what `git status` says about this worktree".
///
/// Every consumer used to spawn its own: the worktree listing, the insights
/// snapshot (twice for the main checkout), the collision scan and `get_status`
/// each read all of the worktrees, one after another, so a refresh paid for
/// each worktree about twice. They now ask here, and a worktree is read once
/// for all of them.
///
/// The rule that keeps this from handing anyone old news: a read is reused
/// only when it **started** at or after the moment the caller says it is not
/// willing to look further back than (`not_before`). A caller's default is the
/// moment it asked (the start of its pass), so it gets nothing it could not
/// have got by reading itself, and a read already under way when it arrives is
/// never handed to it (that read may not have seen what changed just before
/// the caller asked). A caller that has a longer tolerance, the hook-path
/// collision memo's five seconds, says so.
///
/// - A cell is keyed by canonical worktree path and admission class, as the
///   listing flights are: a background reader never serves a user-facing one,
///   and the other way round.
/// - One read at a time per cell. Callers that arrive while one runs wait on
///   it (bounded by their own deadline) and take its answer if it began late
///   enough for them, otherwise read after it.
/// - A failed read, a refused one and one cut by a deadline are never kept. A
///   failure is handed only to the callers that were waiting on that very
///   read, and a deadline's cut to nobody: it was the leader's clock.
/// - Bounded: [`MAX_CELLS`] cells, a cell keeps one answer, and an answer past
///   [`MAX_KEPT_BYTES`] reaches only the callers waiting on its read.
/// - Trust is checked before the cell is consulted, as at the spawn.
mod status_cell {
    use super::{spawn_porcelain, ScanDeadline, DEADLINE_PASSED};
    use crate::engine::git_cli::{current_admission, is_timed_out, Admission};
    use crate::ipc_error::CommandError;
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Condvar, Mutex, OnceLock, PoisonError, TryLockError};
    use std::time::{Duration, Instant};

    pub(super) const MAX_CELLS: usize = 512;
    /// The most one cell keeps. A status of a thousand changed files is a few
    /// tens of kilobytes; a bigger one is read again.
    const MAX_KEPT_BYTES: usize = 256 * 1024;

    /// One status read and when it began.
    #[derive(Clone, Debug)]
    pub(crate) struct PorcelainRead {
        pub(crate) bytes: Arc<str>,
        /// Taken before the child was spawned, so everything the read saw
        /// happened at or after it.
        pub(crate) started: Instant,
        /// This caller's own read made it, rather than another's.
        pub(crate) led: bool,
    }

    struct Fill {
        started: Instant,
        /// The leader ran under a caller's deadline, so a timeout it reports
        /// is that caller's clock; without one it is git's own limit, which
        /// every waiter would hit too.
        deadline_bound: bool,
        result: OnceLock<Result<PorcelainRead, CommandError>>,
    }

    #[derive(Default)]
    struct State {
        held: Option<PorcelainRead>,
        filling: Option<Arc<Fill>>,
    }

    #[derive(Default)]
    struct Cell {
        state: Mutex<State>,
        done: Condvar,
    }

    #[derive(Clone, PartialEq, Eq, Hash)]
    struct Key {
        dir: PathBuf,
        class: Admission,
    }

    #[derive(Default)]
    struct Table {
        cells: HashMap<Key, (u64, Arc<Cell>)>,
        tick: u64,
    }

    fn table() -> &'static Mutex<Table> {
        static TABLE: OnceLock<Mutex<Table>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(Table::default()))
    }

    fn key_for(dir: &Path) -> Key {
        Key {
            dir: dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()),
            class: current_admission(),
        }
    }

    fn cell_for(key: &Key) -> Arc<Cell> {
        let mut table = table().lock().unwrap_or_else(PoisonError::into_inner);
        table.tick += 1;
        let tick = table.tick;
        if let Some((touched, cell)) = table.cells.get_mut(key) {
            *touched = tick;
            return Arc::clone(cell);
        }
        if table.cells.len() >= MAX_CELLS {
            evict_one(&mut table.cells);
        }
        let cell = Arc::new(Cell::default());
        table.cells.insert(key.clone(), (tick, Arc::clone(&cell)));
        cell
    }

    /// Drops one cell that is not being read: a worktree that is gone first,
    /// then the one asked about longest ago.
    fn evict_one(cells: &mut HashMap<Key, (u64, Arc<Cell>)>) {
        let idle = |cell: &Arc<Cell>| match cell.state.try_lock() {
            Ok(state) => state.filling.is_none(),
            Err(TryLockError::Poisoned(poisoned)) => poisoned.into_inner().filling.is_none(),
            Err(TryLockError::WouldBlock) => false,
        };
        let victim = cells
            .iter()
            .filter(|(_, (_, cell))| idle(cell))
            .min_by_key(|(key, (touched, _))| (key.dir.exists(), *touched))
            .map(|(key, _)| key.clone());
        if let Some(victim) = victim {
            cells.remove(&victim);
        }
    }

    fn forget(key: &Key) {
        table()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .cells
            .remove(key);
    }

    pub(super) fn age(dir: &Path) -> Option<Duration> {
        let key = key_for(dir);
        let cell = {
            let table = table().lock().unwrap_or_else(PoisonError::into_inner);
            Arc::clone(&table.cells.get(&key)?.1)
        };
        let state = cell.state.lock().unwrap_or_else(PoisonError::into_inner);
        state.held.as_ref().map(|held| held.started.elapsed())
    }

    /// Whether an error is this caller's or somebody's clock running out,
    /// which no other caller should inherit.
    fn is_a_cut(error: &CommandError) -> bool {
        error.message() == DEADLINE_PASSED || is_timed_out(error.message())
    }

    /// A read that ran out of git's own time limit, as opposed to a deadline
    /// that was never given the chance to start it.
    fn is_a_hang(error: &CommandError) -> bool {
        error.message() != DEADLINE_PASSED && is_timed_out(error.message())
    }

    pub(super) fn read(
        dir: &Path,
        deadline: Option<&ScanDeadline>,
        not_before: Instant,
    ) -> Result<PorcelainRead, CommandError> {
        crate::repository_trust::require(dir)?;
        let key = key_for(dir);
        let cell = cell_for(&key);
        let outcome = read_in(&cell, dir, deadline, not_before);
        if outcome.is_err() && !dir.is_dir() {
            // A worktree that was removed leaves no cell behind.
            forget(&key);
        }
        outcome
    }

    /// Publishes a fill's outcome and frees the cell, also when the leader
    /// panics, so no caller is left waiting on a read that is not running.
    struct Settle<'a> {
        cell: &'a Cell,
        fill: &'a Arc<Fill>,
        settled: bool,
    }

    impl Settle<'_> {
        fn finish(mut self, outcome: Result<PorcelainRead, CommandError>) {
            self.publish(outcome);
        }

        fn publish(&mut self, outcome: Result<PorcelainRead, CommandError>) {
            self.settled = true;
            let mut state = self
                .cell
                .state
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            match &outcome {
                Ok(read) if read.bytes.len() <= MAX_KEPT_BYTES => {
                    state.held = Some(read.clone());
                }
                // A caller's own clock ran out: nothing was learned about the
                // worktree, so what is kept stands.
                Err(error) if is_a_cut(error) => {}
                // Too big to keep, or failed: what was kept is not trusted
                // past a read that could not be made.
                _ => state.held = None,
            }
            let _ = self.fill.result.set(outcome);
            if state
                .filling
                .as_ref()
                .is_some_and(|running| Arc::ptr_eq(running, self.fill))
            {
                state.filling = None;
            }
            self.cell.done.notify_all();
        }
    }

    impl Drop for Settle<'_> {
        fn drop(&mut self) {
            if !self.settled {
                self.publish(Err("the shared status read ended without a result".into()));
            }
        }
    }

    fn read_in(
        cell: &Cell,
        dir: &Path,
        deadline: Option<&ScanDeadline>,
        not_before: Instant,
    ) -> Result<PorcelainRead, CommandError> {
        let mut state = cell.state.lock().unwrap_or_else(PoisonError::into_inner);
        let fill = loop {
            if let Some(held) = &state.held {
                if held.started >= not_before {
                    return Ok(held.clone());
                }
            }
            let Some(running) = state.filling.clone() else {
                let fill = Arc::new(Fill {
                    started: Instant::now(),
                    deadline_bound: deadline.is_some(),
                    result: OnceLock::new(),
                });
                state.filling = Some(Arc::clone(&fill));
                break fill;
            };
            while running.result.get().is_none() {
                state = match deadline {
                    None => cell
                        .done
                        .wait(state)
                        .unwrap_or_else(PoisonError::into_inner),
                    Some(deadline) => {
                        if deadline.expired() {
                            return Err(DEADLINE_PASSED.to_string().into());
                        }
                        cell.done
                            .wait_timeout(state, deadline.remaining())
                            .unwrap_or_else(PoisonError::into_inner)
                            .0
                    }
                };
            }
            // Only a read that began late enough for this caller is its
            // answer; otherwise the cell is free for it to read after.
            if running.started >= not_before {
                match running.result.get() {
                    Some(Ok(read)) => return Ok(read.clone()),
                    Some(Err(error)) if !is_a_cut(error) => return Err(error.clone()),
                    // A hung git ends every waiter at the same moment it ends
                    // the leader, not one 90 s after another. Only a cut that
                    // was a caller's deadline is retried.
                    Some(Err(error)) if !running.deadline_bound && is_a_hang(error) => {
                        return Err(error.clone());
                    }
                    _ => {}
                }
            }
        };
        drop(state);

        let settle = Settle {
            cell,
            fill: &fill,
            settled: false,
        };
        let outcome = spawn_porcelain(dir, deadline, None).map(|text| PorcelainRead {
            bytes: Arc::from(text),
            started: fill.started,
            led: false,
        });
        settle.finish(outcome.clone());
        outcome.map(|read| PorcelainRead { led: true, ..read })
    }
}

/// The paths a limited status read is about.
#[derive(Clone, Copy)]
struct OnlyPaths<'a> {
    paths: &'a [&'a str],
    /// The worktree's volume treats `a` and `A` as one name.
    ignore_case: bool,
}

/// Whether the volume `repo` lives on ignores case, measured on the volume
/// itself: `.git` is there in every worktree, and under a case-insensitive
/// volume `.GIT` is the same file. Git's own `core.ignorecase` is probed the
/// same way at `init`, but reading it would cost a process per worktree.
fn volume_ignores_case(repo: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        match (
            std::fs::metadata(repo.join(".git")),
            std::fs::metadata(repo.join(".GIT")),
        ) {
            (Ok(real), Ok(flipped)) => (real.dev(), real.ino()) == (flipped.dev(), flipped.ino()),
            _ => false,
        }
    }
    #[cfg(not(unix))]
    {
        // Windows volumes ignore case unless a directory opted out.
        let _ = repo;
        true
    }
}

/// The proper ancestors of `path` that are nested repositories in `repo`
/// (a submodule, or any directory with its own `.git`). A status of the file
/// inside one reports nothing, because the superproject sees only the gitlink
/// row, so the gitlink is asked about as well.
fn nested_repository_ancestors(repo: &Path, path: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut prefix = String::new();
    let mut parts = path.split('/').peekable();
    while let Some(part) = parts.next() {
        if parts.peek().is_none() {
            break;
        }
        if !prefix.is_empty() {
            prefix.push('/');
        }
        prefix.push_str(part);
        if repo.join(&prefix).join(".git").exists() {
            found.push(prefix.clone());
        }
    }
    found
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

    static FORCED_TIMEOUTS: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

    /// Whether the unfiltered status read of `dir` is to end as a git timeout.
    pub(super) fn forced_timeout(dir: &Path) -> bool {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        FORCED_TIMEOUTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(&canonical)
    }

    /// Runs `body` with every unfiltered status read of `dir` ending as a git
    /// timeout (after any probe has run).
    pub(crate) fn with_forced_timeout<T>(dir: &Path, body: impl FnOnce() -> T) -> T {
        let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        struct Remove(PathBuf);
        impl Drop for Remove {
            fn drop(&mut self) {
                let mut forced = FORCED_TIMEOUTS
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner);
                if let Some(at) = forced.iter().position(|dir| *dir == self.0) {
                    forced.remove(at);
                }
            }
        }
        FORCED_TIMEOUTS
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(canonical.clone());
        let _remove = Remove(canonical);
        body()
    }

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
pub fn changed_paths(worktree_path: &str) -> Result<(Vec<String>, bool), CommandError> {
    changed_paths_within(worktree_path, None)
}

/// [`changed_paths`] that stops waiting on git when `deadline` passes. A read
/// that is cut returns an error saying so, never an empty list.
pub fn changed_paths_within(
    worktree_path: &str,
    deadline: Option<&ScanDeadline>,
) -> Result<(Vec<String>, bool), CommandError> {
    let not_before = deadline.map_or_else(Instant::now, ScanDeadline::started);
    changed_paths_since(worktree_path, deadline, not_before)
        .map(|read| (read.paths, read.truncated))
}

/// The changed paths of one worktree, and how old the read they came from is.
#[derive(Debug, Clone)]
pub struct ChangedPaths {
    pub paths: Vec<String>,
    pub truncated: bool,
    /// How long ago the status read began: zero for one made for this call.
    pub age: Duration,
}

/// [`changed_paths_within`] that accepts a status read of this worktree begun
/// at or after `not_before`, from whoever made it (see [`status_cell`]).
/// `Instant::now()` asks for a read of the caller's own.
pub fn changed_paths_since(
    worktree_path: &str,
    deadline: Option<&ScanDeadline>,
    not_before: Instant,
) -> Result<ChangedPaths, CommandError> {
    let repo = validate_repo(worktree_path)?;
    if !repo.is_dir() {
        return Err(format!("worktree path is not a directory: {}", repo.display()).into());
    }
    let read = read_status_since(&repo, deadline, not_before)?;
    let (paths, truncated) = bounded_status_paths(read.bytes.as_bytes());
    Ok(ChangedPaths {
        paths,
        truncated,
        age: if read.led {
            Duration::ZERO
        } else {
            read.started.elapsed()
        },
    })
}

/// [`changed_paths_within`] for one path: the entries of a pathspec-limited
/// status of `worktree_path`. Empty means `path` is clean here; an error means
/// unknown, never clean.
pub fn changed_paths_only(
    worktree_path: &str,
    path: &str,
    deadline: Option<&ScanDeadline>,
) -> Result<(Vec<String>, bool), CommandError> {
    let repo = validate_repo(worktree_path)?;
    if !repo.is_dir() {
        return Err(format!("worktree path is not a directory: {}", repo.display()).into());
    }
    let ignore_case = volume_ignores_case(&repo);
    let nested = nested_repository_ancestors(&repo, path);
    let mut asked: Vec<&str> = vec![path];
    asked.extend(nested.iter().map(String::as_str));
    let stdout = spawn_porcelain(
        &repo,
        deadline,
        Some(OnlyPaths {
            paths: &asked,
            ignore_case,
        }),
    )?;
    let (mut paths, truncated) = bounded_status_paths(stdout.as_bytes());
    // A row for the file under another spelling of its name, or for the nested
    // repository that contains it, is a row for this file: the caller matches
    // rows by the path it asked about.
    for entry in &mut paths {
        let same = |other: &str| {
            if ignore_case {
                entry.eq_ignore_ascii_case(other)
            } else {
                entry == other
            }
        };
        if same(path) || nested.iter().any(|ancestor| same(ancestor)) {
            *entry = path.to_string();
        }
    }
    paths.dedup();
    Ok((paths, truncated))
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
pub fn list_worktrees_lite(repo_path: &str) -> Result<Vec<WorktreeInfo>, CommandError> {
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
type BaseRead = Result<Option<(String, String)>, CommandError>;

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
            &format!("{base_oid}...refs/heads/{b}"),
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
    /// The machine's portless routes, read and probed once for the listing.
    /// `None` outside a full listing, where a worktree's scan reads its own.
    routes: Option<PortlessSnapshot>,
}

impl Base {
    fn none() -> Self {
        Self {
            default: Ok(None),
            batch: None,
            routes: None,
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
    let patterns: Vec<String> = branches.iter().map(|b| format!("refs/heads/{b}")).collect();
    let patterns: Vec<&str> = patterns.iter().map(String::as_str).collect();
    let query = crate::engine::git_reader::RefQuery {
        namespaces: &["refs/heads"],
        command: &["for-each-ref"],
        patterns: &patterns,
        fields: "%(refname)",
    };
    let listing =
        crate::engine::git_reader::list_against(repo, &query, Some(base_oid), false).ok()?;
    let mut out = HashMap::new();
    for row in listing.rows().ok()? {
        let Some(branch) = row.fields[0].strip_prefix("refs/heads/") else {
            continue;
        };
        if !branches.contains(&branch) {
            continue;
        }
        if let Some((ahead, behind)) = row.counts {
            out.insert(branch.to_string(), WorktreeDivergence { ahead, behind });
        }
    }
    Some(out)
}

/// How far one checkout's HEAD commit is from the default base: commits it has
/// that the base lacks (`ahead`) and commits the base has that it lacks
/// (`behind`). Counted from the commit, so a detached HEAD has them too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeadDivergence {
    pub ahead: u32,
    pub behind: u32,
}

/// Most `rev-list` reads (cache misses) one [`measure_head_divergence`] pass
/// makes. A HEAD past it is reported unmeasured, not read.
pub const MAX_HEAD_DIVERGENCE_MISSES: usize = 8;

/// Most `(repository, head, base)` answers remembered. The key is immutable (a
/// commit's ancestry never changes), so an entry is never stale; the bound
/// only keeps a long-lived process from growing with every commit it has seen.
const HEAD_DIVERGENCE_CACHE_CAPACITY: usize = 1024;

/// `(repository, shallow boundary, head, base)`. Ancestry is immutable only
/// while history is whole: a shallow clone's counts stop at its boundary, and
/// deepening it changes them for the same head and base, so the boundary file's
/// state is part of the key.
type DivergenceKey = (PathBuf, String, String, String);

/// The state of `<common dir>/shallow` as a key: empty when absent, otherwise
/// its length and modification time (git rewrites the file whenever the
/// boundary moves). Read without spawning. `None` when the common directory
/// or the file could not be read, in which case nothing is cached or served.
fn shallow_token(repo: &Path) -> Option<(String, bool)> {
    let file = resolve_git_common_dir(repo).ok()?.join("shallow");
    match std::fs::metadata(&file) {
        Ok(meta) => {
            let modified = meta
                .modified()
                .ok()
                .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |since| since.as_nanos());
            Some((format!("{}:{modified}", meta.len()), true))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Some((String::new(), false)),
        Err(_) => None,
    }
}

fn head_divergence_cache() -> &'static Mutex<LruMap<DivergenceKey, HeadDivergence>> {
    static CACHE: OnceLock<Mutex<LruMap<DivergenceKey, HeadDivergence>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(LruMap::new(HEAD_DIVERGENCE_CACHE_CAPACITY)))
}

/// What [`measure_head_divergence`] established.
#[derive(Debug, Default)]
pub struct HeadDivergences {
    /// Divergence by full HEAD oid, for every oid that was measured.
    pub by_head: HashMap<String, HeadDivergence>,
    /// Why anything is missing from `by_head`, always starting `unmeasured:`:
    /// no default base resolved, a read failed, the per-pass cap of
    /// [`MAX_HEAD_DIVERGENCE_MISSES`] was spent, or the deadline passed. `None`
    /// when every asked oid has an entry, so an absent count is never mistaken
    /// for zero.
    pub unread: Option<String>,
    /// The repository is a shallow clone: every count is bounded by the clone
    /// depth and is not the whole history's.
    pub shallow: bool,
}

/// Measures each distinct commit in `heads` against the repository's default
/// base, keyed by oid: a detached worktree has no branch for
/// [`measure_main_divergence`] to name, and worktrees on one commit share one
/// read. The base is the one default-base owner
/// ([`crate::engine::git_reader::default_base`]), the same answer the branch
/// list measures against.
///
/// An answer already known for the immutable `(head, base)` pair costs no
/// process. A miss is one `rev-list --left-right --count` through the shared
/// spawn gate (so it spends the same budget as every other read), at most
/// [`MAX_HEAD_DIVERGENCE_MISSES`] per pass and each given what the deadline
/// has left; every HEAD past either limit is unmeasured.
pub fn measure_head_divergence(
    repo: &Path,
    heads: &[&str],
    deadline: &ScanDeadline,
) -> HeadDivergences {
    measure_head_divergence_capped(repo, heads, deadline, MAX_HEAD_DIVERGENCE_MISSES)
}

/// [`measure_head_divergence`] with the per-pass miss cap as an argument, so
/// the cold-cache-over-cap case is reachable without nine worktrees.
pub(crate) fn measure_head_divergence_capped(
    repo: &Path,
    heads: &[&str],
    deadline: &ScanDeadline,
    max_misses: usize,
) -> HeadDivergences {
    let mut out = HeadDivergences::default();
    let mut distinct: Vec<&str> = Vec::new();
    for head in heads.iter().copied().filter(|h| is_full_oid(h)) {
        if !distinct.contains(&head) {
            distinct.push(head);
        }
    }
    if distinct.is_empty() {
        return out;
    }
    let base_oid = match read_base(repo) {
        Ok(Some((_, oid))) => oid,
        Ok(None) => {
            out.unread =
                Some("unmeasured: the repository has no default base to measure against".into());
            return out;
        }
        Err(reason) => {
            out.unread = Some(format!(
                "unmeasured: the default base could not be read: {reason}",
                reason = reason.message()
            ));
            return out;
        }
    };
    let mut misses = 0usize;
    let mut unread = 0usize;
    let mut first_reason = None;
    let boundary = shallow_token(repo);
    out.shallow = boundary.as_ref().is_some_and(|(_, shallow)| *shallow);
    for head in &distinct {
        let key = boundary.as_ref().map(|(token, _)| {
            (
                repo.to_path_buf(),
                token.clone(),
                (*head).to_string(),
                base_oid.clone(),
            )
        });
        let cached = key.as_ref().and_then(|key| {
            head_divergence_cache()
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(key)
                .copied()
        });
        if let Some(known) = cached {
            out.by_head.insert((*head).to_string(), known);
            continue;
        }
        let reason = if misses >= max_misses {
            format!("past the per-pass limit of {max_misses} reads")
        } else if deadline.expired() {
            "the deadline passed".to_string()
        } else {
            misses += 1;
            match left_right_counts(repo, &base_oid, head, deadline.remaining()) {
                Ok(counts) => {
                    if let Some(key) = key {
                        head_divergence_cache()
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .insert(key, counts);
                    }
                    out.by_head.insert((*head).to_string(), counts);
                    continue;
                }
                Err(reason) => reason.into_message(),
            }
        };
        unread += 1;
        first_reason.get_or_insert(reason);
    }
    if let Some(reason) = first_reason {
        out.unread = Some(format!(
            "unmeasured: {unread} of {} distinct HEAD commits not measured: {reason}",
            distinct.len()
        ));
    }
    out
}

fn left_right_counts(
    repo: &Path,
    base_oid: &str,
    head_oid: &str,
    timeout: Duration,
) -> Result<HeadDivergence, CommandError> {
    let stdout = git_text_with_timeout(
        repo,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base_oid}...{head_oid}"),
        ],
        timeout,
    )?;
    let mut parts = stdout.split_whitespace();
    let behind = parts.next().and_then(|n| n.parse().ok());
    let ahead = parts.next().and_then(|n| n.parse().ok());
    match (ahead, behind) {
        (Some(ahead), Some(behind)) => Ok(HeadDivergence { ahead, behind }),
        _ => Err(format!("unreadable rev-list counts: {:?}", stdout.trim()).into()),
    }
}

/// Lists every worktree of the repository, main entry first, with dirty-file
/// counts, diff deltas, and detected portless routes for the worktrees closest to the front.
pub fn list_worktrees(repo_path: &str) -> Result<Vec<WorktreeInfo>, CommandError> {
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
) -> Result<Vec<WorktreeInfo>, CommandError> {
    if depth == ScanDepth::Listing {
        return scan_listing(repo_path, depth, Instant::now());
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
                state.latest_arrival = Instant::now();
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
    let (depth, latest_arrival) = {
        let mut lanes = flight_lanes()
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let lane = lanes.entry(key.clone()).or_default();
        lane.open = None;
        lane.running = Some(Arc::clone(&flight));
        let state = flight.state.lock().unwrap_or_else(PoisonError::into_inner);
        (state.depth, state.latest_arrival)
    };

    /// Publishes whatever the leader ended with and frees the lane, so a
    /// leader that panics cannot strand the callers waiting on it.
    struct Land<'a> {
        key: &'a FlightKey,
        flight: &'a Arc<Flight>,
        outcome: Option<Result<Vec<WorktreeInfo>, CommandError>>,
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
    // Every caller sharing this listing had arrived by `latest_arrival`, so a
    // status read begun after it is no older than any of their questions.
    let outcome = scan_listing(repo_path, depth, latest_arrival);
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
    /// When the last caller asked, the leader or a joiner.
    latest_arrival: Instant,
    result: Option<Result<Vec<WorktreeInfo>, CommandError>>,
}

impl Flight {
    fn new(depth: ScanDepth) -> Self {
        Self {
            state: Mutex::new(FlightState {
                depth,
                latest_arrival: Instant::now(),
                result: None,
            }),
            done: Condvar::new(),
        }
    }

    fn wait(&self) -> Result<Vec<WorktreeInfo>, CommandError> {
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

    fn publish(&self, outcome: Result<Vec<WorktreeInfo>, CommandError>) {
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
fn scan_listing(
    repo_path: &str,
    depth: ScanDepth,
    not_before: Instant,
) -> Result<Vec<WorktreeInfo>, CommandError> {
    let repo = validate_repo(repo_path)?;
    let pass = ScanPass::asked_at(not_before);
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
    // Asked once for the whole listing, whenever any scanned worktree is on a
    // branch: without the base no branch is known to be it, so even a lone
    // checkout of the default needs the answer to be called "no divergence".
    let needs_base =
        depth == ScanDepth::Full && scan_targets.iter().any(|&idx| parsed[idx].branch.is_some());
    let mut base = if needs_base {
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
        Base {
            default,
            batch,
            routes: None,
        }
    } else {
        Base::none()
    };
    // Routes belong to the machine, not to a worktree: one read and one probe
    // per port for the whole listing.
    if depth == ScanDepth::Full {
        base.routes = Some(PortlessSnapshot::load());
    }
    let mut metrics = scan_in_waves(&scan_targets, class, None, |idx| {
        scan_worktree(&repo, &parsed[idx], depth, &base, None, &pass)
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
            &ScanPass::now(),
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
    // The pass began when its clock did, so a status any stage of it read since
    // (the main checkout's, for one) is this stage's answer too.
    let pass = ScanPass::asked_at(deadline.started());
    let mut scans = scan_in_waves(&targets, class, Some(deadline), |idx| {
        // The repository path is read only at `Full` depth.
        scan_worktree(
            Path::new(&entries[idx].path),
            &entries[idx],
            ScanDepth::Dirty,
            &Base::none(),
            Some(deadline),
            &pass,
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

/// What one pass over a repository's worktrees shares between its stages and
/// its worker threads: how far back a status read may reach, and the clock
/// readings a shortstat stamp is judged on. Taken on the thread that starts the
/// pass, because the clock a test installs is that thread's own and the worker
/// threads of a wave would not see it.
#[derive(Clone, Copy)]
struct ScanPass {
    /// The status of a worktree is taken from any read that began at or after
    /// this: the moment the pass's caller asked, so a read is never older than
    /// the question.
    not_before: Instant,
    mono: Instant,
    wall: std::time::SystemTime,
    racy: Duration,
    real: Instant,
}

impl ScanPass {
    fn asked_at(not_before: Instant) -> Self {
        Self {
            not_before,
            mono: crate::engine::ref_cache::mono_now(),
            wall: crate::engine::ref_cache::wall_now(),
            racy: crate::engine::ref_cache::racy_window(),
            real: Instant::now(),
        }
    }

    fn now() -> Self {
        Self::asked_at(Instant::now())
    }

    fn mono_now(&self) -> Instant {
        self.mono + self.real.elapsed()
    }

    fn wall_now(&self) -> std::time::SystemTime {
        self.wall + self.real.elapsed()
    }
}

/// `git diff HEAD --shortstat` of a worktree, skipped when everything it is a
/// function of is provably what it was the last time it ran.
///
/// The measurement is a function of HEAD's commit, the index, the files git
/// reports as changed, and the config and attributes that shape a diff. A
/// stamp holds an identity for each (HEAD oid, the pass's status bytes
/// byte-for-byte, the index, every changed path's device, inode, size, mtime
/// and ctime including both sides of a rename, the repository's and the user's
/// config), and the previous answer is reused only when all of them match.
///
/// What keeps a stale answer from being served:
///
/// - The stamp is captured before the diff runs and again after it, and the
///   answer is kept only when the two are equal: a write that raced the child
///   leaves no entry.
/// - Every timestamp in it must be older than the racy window (the one
///   `ref_cache` uses): two writes inside one timestamp tick cannot be told
///   apart, and ctime is what catches an edit that restored mtime and size.
/// - A stamp that cannot be proven yields no skip and no entry: a changed path
///   that is a directory (a submodule, whose own commits move nothing here), a
///   path that is unreadable or absent without a deletion to explain it, a name
///   that is not UTF-8, an untracked `.gitattributes`, a HEAD that is not a
///   plain loose or packed ref, more changed paths than [`MAX_STAMPED_PATHS`],
///   a file system without a change time, any platform that is not unix.
/// - An entry lives [`DIFF_STAMP_TTL`] whatever the stamp says, which bounds
///   the inputs no stamp sees (files a config `include`s). Only measured
///   successes are kept, and at most [`MAX_DIFF_STAMPS`] at once.
mod diff_stamp {
    use super::{ScanPass, WorktreeDiffStat};
    use crate::engine::ref_cache::{user_config_paths, ChangeStamp};
    use std::collections::HashMap;
    use std::path::{Path, PathBuf};
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::time::{Duration, Instant};

    pub(super) const DIFF_STAMP_TTL: Duration = Duration::from_secs(60);
    pub(super) const MAX_DIFF_STAMPS: usize = 64;
    const MAX_STAMPED_PATHS: usize = 1024;

    #[derive(Debug, PartialEq, Eq)]
    pub(super) struct DiffStamp {
        head: String,
        status: Arc<str>,
        index: Option<ChangeStamp>,
        files: Vec<(String, Option<ChangeStamp>)>,
        config: Vec<(PathBuf, Option<ChangeStamp>)>,
    }

    struct Kept {
        stamp: DiffStamp,
        stat: WorktreeDiffStat,
        stored: Instant,
    }

    fn table() -> &'static Mutex<HashMap<PathBuf, Kept>> {
        static TABLE: OnceLock<Mutex<HashMap<PathBuf, Kept>>> = OnceLock::new();
        TABLE.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn key(dir: &Path) -> PathBuf {
        dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf())
    }

    /// The measurement for `dir`, from memory when the stamp proves it, else
    /// from `measure` (kept when it succeeded and nothing moved meanwhile).
    pub(super) fn through(
        dir: &Path,
        status: &Arc<str>,
        pass: &ScanPass,
        measure: impl FnOnce() -> Option<WorktreeDiffStat>,
    ) -> Option<WorktreeDiffStat> {
        let key = key(dir);
        let Some(before) = DiffStamp::capture(dir, status, pass) else {
            forget(&key);
            return measure();
        };
        {
            let table = table().lock().unwrap_or_else(PoisonError::into_inner);
            if let Some(kept) = table.get(&key) {
                let young = pass
                    .mono_now()
                    .checked_duration_since(kept.stored)
                    .is_some_and(|age| age < DIFF_STAMP_TTL);
                if young && kept.stamp == before {
                    return Some(kept.stat.clone());
                }
            }
        }
        let measured = measure();
        match &measured {
            Some(stat) if DiffStamp::capture(dir, status, pass).as_ref() == Some(&before) => {
                let mut table = table().lock().unwrap_or_else(PoisonError::into_inner);
                if table.len() >= MAX_DIFF_STAMPS && !table.contains_key(&key) {
                    let oldest = table
                        .iter()
                        .min_by_key(|(_, kept)| kept.stored)
                        .map(|(key, _)| key.clone());
                    if let Some(oldest) = oldest {
                        table.remove(&oldest);
                    }
                }
                table.insert(
                    key,
                    Kept {
                        stamp: before,
                        stat: stat.clone(),
                        stored: pass.mono_now(),
                    },
                );
            }
            _ => forget(&key),
        }
        measured
    }

    fn forget(key: &Path) {
        table()
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(key);
    }

    impl DiffStamp {
        #[cfg(not(unix))]
        pub(super) fn capture(_dir: &Path, _status: &Arc<str>, _pass: &ScanPass) -> Option<Self> {
            None
        }

        #[cfg(unix)]
        pub(super) fn capture(dir: &Path, status: &Arc<str>, pass: &ScanPass) -> Option<Self> {
            let (private, common) = crate::repository_trust::git_directories(dir).ok()?;
            let head = head_oid(&private, &common)?;
            let rows = changed_rows(status.as_bytes())?;
            let mut files = Vec::with_capacity(rows.len());
            for (path, may_be_absent) in rows {
                let full = dir.join(&path);
                match std::fs::symlink_metadata(&full) {
                    Ok(meta) if meta.is_dir() => return None,
                    Ok(meta) => files.push((path, Some(ChangeStamp::of(&meta)))),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound && may_be_absent => {
                        files.push((path, None));
                    }
                    Err(_) => return None,
                }
            }
            let index = ChangeStamp::at(&private.join("index"));
            let mut config_paths = vec![
                common.join("config"),
                private.join("config.worktree"),
                common.join("info").join("attributes"),
            ];
            config_paths.extend(user_config_paths());
            // Attributes files git reads for every diff besides the worktree's
            // own: the user's, wherever `XDG_CONFIG_HOME` puts it (or its
            // `~/.config` default), and the one `core.attributesFile` names.
            if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
                config_paths.push(PathBuf::from(xdg).join("git").join("attributes"));
            }
            if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
                config_paths.push(
                    PathBuf::from(home)
                        .join(".config")
                        .join("git")
                        .join("attributes"),
                );
            }
            let named = named_attributes_files(&config_paths)?;
            config_paths.extend(named);
            let config: Vec<(PathBuf, Option<ChangeStamp>)> = config_paths
                .into_iter()
                .map(|path| {
                    let identity = ChangeStamp::at(&path);
                    (path, identity)
                })
                .collect();

            let wall = pass.wall_now();
            let settled = |identity: &ChangeStamp| {
                identity
                    .newest_timestamp()
                    .and_then(|newest| wall.duration_since(newest).ok())
                    .is_some_and(|age| age >= pass.racy)
            };
            let all_settled = files
                .iter()
                .filter_map(|(_, identity)| identity.as_ref())
                .chain(index.as_ref())
                .chain(config.iter().filter_map(|(_, identity)| identity.as_ref()))
                .all(settled);
            all_settled.then_some(Self {
                head,
                status: Arc::clone(status),
                index,
                files,
                config,
            })
        }
    }

    /// The files `core.attributesFile` names in any of `configs`, to be stamped
    /// beside them. `None` (unprovable) when a config sets the key in a form
    /// read here with doubt: no `=`, a relative path, or a value carrying a
    /// quote, escape or comment character. A config that cannot be read is
    /// also unprovable; one that is absent has no key.
    #[cfg(unix)]
    fn named_attributes_files(configs: &[PathBuf]) -> Option<Vec<PathBuf>> {
        let mut named = Vec::new();
        for config in configs {
            let text = match std::fs::read_to_string(config) {
                Ok(text) => text,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(_) => return None,
            };
            for line in text.lines() {
                let line = line.trim();
                let lower = line.to_ascii_lowercase();
                let Some(rest) = lower.strip_prefix("attributesfile") else {
                    continue;
                };
                if !rest.starts_with(|c: char| c.is_whitespace() || c == '=') {
                    continue;
                }
                let value = line["attributesfile".len()..].trim_start();
                let value = value.strip_prefix('=')?.trim();
                if value.is_empty() || value.contains(['"', '\\', '#', ';']) {
                    return None;
                }
                let path = match value.strip_prefix("~/") {
                    Some(tail) => PathBuf::from(std::env::var_os("HOME")?).join(tail),
                    None => PathBuf::from(value),
                };
                if !path.is_absolute() {
                    return None;
                }
                named.push(path);
            }
        }
        Some(named)
    }

    /// The paths a shortstat of HEAD depends on, from `status --porcelain -z`,
    /// each with whether its absence is explained by a deletion or a rename.
    /// Untracked and ignored rows are not in a diff of HEAD (their appearance
    /// changes the status bytes, which are in the stamp). `None` is
    /// unprovable.
    #[cfg(unix)]
    fn changed_rows(bytes: &[u8]) -> Option<Vec<(String, bool)>> {
        let mut rows = Vec::new();
        let mut i = 0usize;
        let field = |from: usize| -> Option<(String, usize)> {
            let n = bytes[from..].iter().position(|&b| b == 0)?;
            let text = std::str::from_utf8(&bytes[from..from + n]).ok()?;
            Some((text.to_string(), from + n + 1))
        };
        while i + 3 <= bytes.len() {
            let (x, y) = (bytes[i] as char, bytes[i + 1] as char);
            let (path, next) = field(i + 3)?;
            i = next;
            if x == '?' && y == '?' {
                // A `.gitattributes` that is not tracked changes how tracked
                // files diff, and its own edits move no status byte.
                if path.rsplit('/').next() == Some(".gitattributes") {
                    return None;
                }
                continue;
            }
            if x == '!' {
                continue;
            }
            rows.push((path, x == 'D' || y == 'D'));
            if x == 'R' || x == 'C' || y == 'R' || y == 'C' {
                let (origin, next) = field(i)?;
                i = next;
                rows.push((origin, true));
            }
            if rows.len() > MAX_STAMPED_PATHS {
                return None;
            }
        }
        Some(rows)
    }

    /// HEAD's commit read from files, without a process: a detached oid, or a
    /// branch's loose or packed ref. `None` for anything else (an unborn
    /// branch, a symbolic ref chain, a reftable store).
    #[cfg(unix)]
    fn head_oid(private: &Path, common: &Path) -> Option<String> {
        let head = std::fs::read_to_string(private.join("HEAD")).ok()?;
        let head = head.trim();
        let Some(name) = head.strip_prefix("ref: ") else {
            return is_oid(head).then(|| head.to_string());
        };
        if name.is_empty()
            || name.starts_with('/')
            || name.split('/').any(|part| part.is_empty() || part == "..")
            || name.contains('\0')
        {
            return None;
        }
        for dir in [private, common] {
            if let Ok(text) = std::fs::read_to_string(dir.join(name)) {
                let text = text.trim();
                return is_oid(text).then(|| text.to_string());
            }
        }
        let packed = std::fs::read_to_string(common.join("packed-refs")).ok()?;
        packed.lines().find_map(|line| {
            let (oid, refname) = line.split_once(' ')?;
            (refname == name && is_oid(oid)).then(|| oid.to_string())
        })
    }

    #[cfg(unix)]
    fn is_oid(text: &str) -> bool {
        matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
    }
}

fn scan_worktree(
    repo: &Path,
    entry: &ParsedWorktree,
    depth: ScanDepth,
    base: &Base,
    deadline: Option<&ScanDeadline>,
    pass: &ScanPass,
) -> WorktreeScan {
    let dir = Path::new(&entry.path);
    if !dir.is_dir() {
        let note = "the worktree directory is missing".to_string();
        return WorktreeScan {
            status_read: StatusRead::Failed(note.clone().into()),
            note: Some(note),
            ..WorktreeScan::default()
        };
    }
    // The measurements below swallow their own errors as "absent"; the
    // failure counter is what says whether one was a refusal rather than a
    // real absence (no upstream, no main branch).
    let failures = crate::engine::git_cli::process_failures();
    let status = match read_status_since(dir, deadline, pass.not_before) {
        Ok(read) => read.bytes,
        Err(reason) if reason == DEADLINE_PASSED => return WorktreeScan::cut_off(),
        Err(reason) => {
            return WorktreeScan {
                status_read: StatusRead::Failed(reason.clone()),
                note: Some(format!(
                    "status could not be read: {reason}",
                    reason = reason.message()
                )),
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
            diff_stamp::through(dir, &status, pass, || measure_diff_stat(dir))
        },
        divergence,
        routes: match &base.routes {
            Some(snapshot) => snapshot.detect(&entry.path, branch),
            None => detect_worktree_routes(&entry.path, branch),
        },
        note: None,
        status_read,
        cut: false,
    };
    // The listing's one default-branch lookup failed outside this scan's
    // failure window, so it is named here or not at all. Without the base no
    // branch is known to be it, so every branch's divergence is unmeasured.
    if let (Err(reason), Some(_)) = (&base.default, branch) {
        scan.note = Some(format!(
            "some measurements were not taken: {reason}",
            reason = reason.message()
        ));
    }
    if crate::engine::git_cli::process_failures() != failures {
        scan.note = Some(format!(
            "some measurements were not taken: {}",
            crate::engine::git_cli::last_process_failure()
                .map(CommandError::into_message)
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
) -> Result<String, CommandError> {
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

    #[cfg(target_os = "macos")]
    if let Some(created_path) =
        try_add_worktree_cow_macos(&repo, target_path, new_branch, start_point, detach)?
    {
        propagate_claude_account_marker(&repo, Path::new(&created_path));
        return Ok(created_path);
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
    propagate_claude_account_marker(&repo, Path::new(target_path));
    Ok(target_path.to_string())
}

const CLAUDE_ACCOUNT_FILE: &str = ".claude-account";
const CLAUDE_ACCOUNT_EXCLUDE_MARKER: &str =
    "# GitPulse: Claude seat marker (local per-checkout, never committed)";
const MAX_CLAUDE_ACCOUNT_BYTES: u64 = 256;
const MAX_CLAUDE_SEAT_LEN: usize = 128;

fn read_validated_claude_account(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::symlink_metadata(path).ok()?;
    if meta.file_type().is_symlink() || !meta.is_file() {
        return None;
    }
    if meta.len() == 0 || meta.len() > MAX_CLAUDE_ACCOUNT_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.len() as u64 > MAX_CLAUDE_ACCOUNT_BYTES {
        return None;
    }
    let text = std::str::from_utf8(&bytes).ok()?;
    let trimmed = text.trim_ascii();
    if trimmed.is_empty()
        || trimmed.len() > MAX_CLAUDE_SEAT_LEN
        || !trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
    {
        return None;
    }
    Some(bytes)
}

/// Propagates a regular, non-symlink `.claude-account` seat file into a newly
/// created linked worktree after confirming `$GIT_COMMON_DIR/info/exclude`
/// hides `/.claude-account`, so `git status --porcelain` stays clean in both
/// main and linked checkouts without ever touching tracked `.gitignore`.
fn propagate_claude_account_marker(source_repo: &Path, new_worktree: &Path) {
    use std::io::Write;

    let common_anchor_file = crate::engine::git_cli::resolve_git_common_dir(source_repo)
        .ok()
        .filter(|common| common.file_name().is_some_and(|n| n == ".git"))
        .and_then(|common| common.parent().map(|root| root.join(CLAUDE_ACCOUNT_FILE)));
    let direct = source_repo.join(CLAUDE_ACCOUNT_FILE);
    let source_is_linked_worktree = source_repo.join(".git").is_file();

    let raw = if source_is_linked_worktree {
        common_anchor_file
            .as_deref()
            .and_then(read_validated_claude_account)
            .or_else(|| read_validated_claude_account(&direct))
    } else {
        read_validated_claude_account(&direct).or_else(|| {
            common_anchor_file
                .as_deref()
                .and_then(read_validated_claude_account)
        })
    };
    let Some(bytes) = raw else {
        return;
    };

    let dst = new_worktree.join(CLAUDE_ACCOUNT_FILE);
    match std::fs::symlink_metadata(&dst) {
        Ok(_) => return,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return,
    }

    let exclude = crate::devmap::init::ensure_file_excluded(
        new_worktree,
        CLAUDE_ACCOUNT_FILE,
        CLAUDE_ACCOUNT_EXCLUDE_MARKER,
    );
    if !exclude.is_clean() {
        return;
    }

    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    if let Ok(mut file) = opts.open(&dst) {
        let _ = file.write_all(&bytes);
        let _ = file.flush();
    }
}

#[cfg(target_os = "macos")]
fn parse_ls_files_stage0(raw: &[u8]) -> Vec<(String, String, String)> {
    let mut entries = Vec::new();
    for rec in raw.split(|&b| b == 0) {
        if rec.is_empty() {
            continue;
        }
        let Ok(rec_str) = std::str::from_utf8(rec) else {
            continue;
        };
        let Some((meta, rel_path)) = rec_str.split_once('\t') else {
            continue;
        };
        let mut parts = meta.split_whitespace();
        let (Some(mode), Some(oid), Some(stage)) = (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if stage == "0" && !rel_path.is_empty() {
            entries.push((mode.to_string(), oid.to_string(), rel_path.to_string()));
        }
    }
    entries
}

#[cfg(target_os = "macos")]
fn parse_porcelain_dirty_paths_z(raw: &[u8]) -> std::collections::HashSet<String> {
    let mut dirty = std::collections::HashSet::new();
    let fields: Vec<&[u8]> = raw.split(|&b| b == 0).collect();
    let mut i = 0usize;
    while i < fields.len() {
        let rec = fields[i];
        if rec.len() < 4 {
            i += 1;
            continue;
        }
        let x = rec[0];
        if let Ok(path) = std::str::from_utf8(&rec[3..]) {
            dirty.insert(path.to_string());
        }
        if matches!(x, b'R' | b'C') && i + 1 < fields.len() {
            if let Ok(orig) = std::str::from_utf8(fields[i + 1]) {
                if !orig.is_empty() {
                    dirty.insert(orig.to_string());
                }
            }
            i += 2;
        } else {
            i += 1;
        }
    }
    dirty
}

#[cfg(target_os = "macos")]
fn try_apfs_clone_regular_file(src: &Path, dst: &Path) -> bool {
    let (Some(src_s), Some(dst_s)) = (src.to_str(), dst.to_str()) else {
        return false;
    };
    let (Ok(src_c), Ok(dst_c)) = (std::ffi::CString::new(src_s), std::ffi::CString::new(dst_s))
    else {
        return false;
    };
    // SAFETY: `src_c` and `dst_c` are valid NUL-terminated C strings.
    if unsafe { libc::clonefile(src_c.as_ptr(), dst_c.as_ptr(), 0) } == 0 {
        true
    } else {
        let _ = std::fs::remove_file(dst);
        false
    }
}

/// Zero-copy APFS tracked source materialization (`F15` / `P3.2`):
/// creates the linked worktree with `--no-checkout`, `clonefile(2)`s clean tracked
/// regular files whose `(mode, blob_oid)` matches `HEAD`, materializes only dirty,
/// diverged, or symlink entries via `git checkout-index`, refreshes the index stat
/// cache, and verifies zero working-tree drift via `git diff-files`.
#[cfg(target_os = "macos")]
fn try_add_worktree_cow_macos(
    repo: &Path,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
) -> Result<Option<String>, CommandError> {
    use std::os::unix::fs::MetadataExt;

    if !repo.join(".git").exists() || (new_branch.is_none() && !detach) {
        return Ok(None);
    }
    let target = Path::new(target_path);
    if target.symlink_metadata().is_ok() {
        return Ok(None);
    }
    let Some(mut ancestor) = target.parent() else {
        return Ok(None);
    };
    while !ancestor.exists() {
        let Some(p) = ancestor.parent() else {
            return Ok(None);
        };
        ancestor = p;
    }
    let (Ok(repo_meta), Ok(anc_meta)) = (std::fs::metadata(repo), std::fs::metadata(ancestor))
    else {
        return Ok(None);
    };
    if repo_meta.dev() != anc_meta.dev() {
        return Ok(None);
    }

    let Ok(ls_donor) = crate::engine::git_cli::git_captured(repo, &["ls-files", "-s", "-z"]) else {
        return Ok(None);
    };
    if ls_donor.status_code != 0 {
        return Ok(None);
    }
    let donor_entries = parse_ls_files_stage0(&ls_donor.stdout);
    if donor_entries.is_empty() {
        return Ok(None);
    }

    for (_, _, rel) in &donor_entries {
        if rel == ".gitmodules" || rel.ends_with("/.gitmodules") {
            return Ok(None);
        }
        if rel == ".gitattributes" || rel.ends_with("/.gitattributes") {
            if let Ok(attr_text) = std::fs::read_to_string(repo.join(rel)) {
                if attr_text.contains("filter=")
                    || attr_text.contains("working-tree-encoding")
                    || attr_text.contains("eol=")
                    || attr_text.contains("ident")
                {
                    return Ok(None);
                }
            }
        }
    }

    let Ok(status_donor) = crate::engine::git_cli::git_captured(
        repo,
        &["status", "--porcelain=v1", "-z", "--untracked-files=no"],
    ) else {
        return Ok(None);
    };
    if status_donor.status_code != 0 {
        return Ok(None);
    }
    let dirty_paths = parse_porcelain_dirty_paths_z(&status_donor.stdout);

    let mut donor_clean: HashMap<String, (String, String)> =
        HashMap::with_capacity(donor_entries.len());
    let mut probe_src: Option<PathBuf> = None;
    for (mode, oid, rel) in donor_entries {
        if dirty_paths.contains(&rel) || !matches!(mode.as_str(), "100644" | "100755") {
            continue;
        }
        if probe_src.is_none() {
            let cand = repo.join(&rel);
            if cand.is_file() {
                probe_src = Some(cand);
            }
        }
        donor_clean.insert(rel, (mode, oid));
    }
    let Some(probe_file) = probe_src else {
        return Ok(None);
    };

    // Probe APFS `clonefile(2)` once in `ancestor` before running any git write.
    let probe_dst = ancestor.join(format!(
        ".gp-cow-probe-{}-{}",
        std::process::id(),
        donor_clean.len()
    ));
    if !try_apfs_clone_regular_file(&probe_file, &probe_dst) {
        return Ok(None);
    }
    let _ = std::fs::remove_file(&probe_dst);

    let mut no_checkout_args: Vec<&str> = vec!["worktree", "add", "--no-checkout"];
    if let Some(branch) = new_branch {
        no_checkout_args.push("-b");
        no_checkout_args.push(branch);
    } else if detach {
        no_checkout_args.push("--detach");
    }
    no_checkout_args.push(target_path);
    if let Some(start) = start_point {
        no_checkout_args.push(start);
    }
    git_text(repo, &no_checkout_args)?;

    let materialize = (|| -> Result<(), CommandError> {
        git_text(target, &["read-tree", "HEAD"])?;
        let ls_target = crate::engine::git_cli::git_captured(target, &["ls-files", "-s", "-z"])?;
        if ls_target.status_code != 0 {
            return Err("git ls-files failed in new worktree".to_string().into());
        }
        let target_entries = parse_ls_files_stage0(&ls_target.stdout);

        let mut created_dirs: std::collections::HashSet<PathBuf> = std::collections::HashSet::new();
        created_dirs.insert(target.to_path_buf());
        let mut need_checkout: Vec<String> = Vec::new();

        for (mode, oid, rel) in target_entries {
            let can_clone = matches!(mode.as_str(), "100644" | "100755")
                && donor_clean
                    .get(&rel)
                    .is_some_and(|(d_mode, d_oid)| d_mode == &mode && d_oid == &oid);
            if !can_clone {
                need_checkout.push(rel);
                continue;
            }
            let dst = target.join(&rel);
            if let Some(parent) = dst.parent() {
                if !created_dirs.contains(parent) {
                    std::fs::create_dir_all(parent).map_err(|e| {
                        CommandError::from(format!("Cannot create '{}': {e}", parent.display()))
                    })?;
                    created_dirs.insert(parent.to_path_buf());
                }
            }
            let src = repo.join(&rel);
            if !try_apfs_clone_regular_file(&src, &dst) {
                need_checkout.push(rel);
            }
        }

        if need_checkout.len() > 512 {
            git_text(target, &["checkout", "-f", "-q", "HEAD"])?;
            return Ok(());
        }
        for chunk in need_checkout.chunks(128) {
            let mut args = Vec::with_capacity(4 + chunk.len());
            args.extend_from_slice(&["checkout-index", "-f", "-q", "--"]);
            for rel in chunk {
                args.push(rel.as_str());
            }
            git_text(target, &args)?;
        }

        let _ = crate::engine::git_cli::git_captured(target, &["update-index", "-q", "--refresh"]);
        let drifted =
            crate::engine::git_cli::git_captured(target, &["diff-files", "--name-only", "-z"])?;
        if drifted.status_code == 0 && !drifted.stdout.is_empty() {
            let mut heal_paths = Vec::new();
            for raw in drifted.stdout.split(|&b| b == 0) {
                if !raw.is_empty() {
                    if let Ok(rel) = std::str::from_utf8(raw) {
                        heal_paths.push(rel.to_string());
                    }
                }
            }
            if !heal_paths.is_empty() {
                for chunk in heal_paths.chunks(128) {
                    let mut args = Vec::with_capacity(4 + chunk.len());
                    args.extend_from_slice(&["checkout-index", "-f", "-q", "--"]);
                    for rel in chunk {
                        args.push(rel.as_str());
                    }
                    git_text(target, &args)?;
                }
                let _ = crate::engine::git_cli::git_captured(
                    target,
                    &["update-index", "-q", "--refresh"],
                );
            }
        }
        Ok(())
    })();

    if materialize.is_err() {
        // Self-heal in place via full checkout so a partial CoW materialization never fails worktree creation.
        git_text(target, &["checkout", "-f", "-q", "HEAD"])?;
    }

    Ok(Some(target_path.to_string()))
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
    /// Caches skipped for want of disk (free space below the floor, or a copy
    /// over the byte cap). Advice, not a failure: the worktree was made.
    #[serde(default)]
    pub low_disk: Option<crate::sysload::LowDisk>,
    /// The volume's free space could not be read, so nothing was skipped on
    /// its account and nothing is known to be plentiful.
    #[serde(default)]
    pub disk_unknown: bool,
    /// A `post_create` hook failed, timed out, or was refused.
    pub hook_error: Option<String>,
    /// Hooks were asked for and the repository's file lists some that were not
    /// approved, so none ran. Not a failure: the worktree is as plain as one
    /// made without hooks, and this says why.
    #[serde(default)]
    pub hook_notice: Option<String>,
    /// What provisioning the DevMap store did: `not_requested` unless the
    /// caller asked for it, and never `provisioned` for a store that is not there.
    #[serde(default)]
    pub devmap: DevmapOutcome,
}

/// Whether, and how, creating a worktree also seeds its DevMap store. Seeding
/// copies a database and syncs an overlay, so it is opt-in: [`Self::None`] is
/// what every caller that did not ask for it gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DevmapMode {
    None,
    /// Copy (or CoW-clone) the anchor's store into the worktree.
    Copy,
    /// Publish an immutable `0444` shared base snapshot + `stack_lease.json`.
    Stacked,
    /// Point at the anchor's store (`base_store_ref.json`) instead of copying it.
    ZeroCopy,
}

/// What seeding a worktree's DevMap store actually did.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DevmapOutcome {
    #[default]
    NotRequested,
    Provisioned {
        zero_copy: bool,
    },
    Skipped {
        reason: String,
    },
    Failed {
        error: String,
    },
}

impl DevmapOutcome {
    fn from_provision(
        result: Result<crate::codeintel::DevmapWorktreeProvision, CommandError>,
    ) -> Self {
        match result {
            Ok(p) if p.seeded => Self::Provisioned {
                zero_copy: p.zero_copy_base_ref,
            },
            Ok(p) => Self::Skipped {
                reason: p.reason.unwrap_or_else(|| "not seeded".to_string()),
            },
            Err(error) => Self::Failed {
                error: error.into_message(),
            },
        }
    }

    /// A seeding that was asked for and did not leave a store.
    pub fn error(&self) -> Option<&str> {
        match self {
            Self::Failed { error } => Some(error),
            _ => None,
        }
    }
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
) -> Result<WorktreeCreated, CommandError> {
    let selection = crate::engine::cow_clone::CacheSelection::default();
    add_worktree_cached(
        repo_path,
        target_path,
        new_branch,
        start_point,
        detach,
        cow_caches.then_some(&selection),
        run_hooks,
    )
}

/// A git write inside a multi-step worktree setup, handed to the command gate
/// before it runs. The arguments exclude the program (`git`). A refusal stops
/// the sequence; the engine never runs a git write nobody judged.
pub type GitWriteGate<'a> = &'a mut dyn FnMut(&[&str]) -> Result<(), CommandError>;

/// Creates a linked worktree with optional Copy-on-Write cache cloning, its
/// DevMap state as `devmap` asks (a CoW-cloned store, the zero-copy pointer
/// `base_store_ref.json`, or none), optional DevMap-guided `--cone` sparse
/// checkout paths, and approved `post_create` hooks.
/// `gate` judges every git write of the sparse sequence (the plain add is
/// judged by the caller before this is called).
#[allow(clippy::too_many_arguments)]
pub fn add_worktree_configured(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    cow_caches: bool,
    run_hooks: bool,
    devmap: DevmapMode,
    sparse_paths: Option<&[String]>,
    gate: GitWriteGate<'_>,
) -> Result<WorktreeCreated, CommandError> {
    // A sparse cone gets DevMap state either way: the pointer when asked for
    // it, otherwise a local CoW store (see `add_sparse_worktree_zero_copy`).
    let zero_copy_devmap = matches!(devmap, DevmapMode::ZeroCopy | DevmapMode::Stacked);
    let cleaned_sparse: Vec<String> = sparse_paths
        .unwrap_or(&[])
        .iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if !cleaned_sparse.is_empty() {
        let sparse = add_sparse_worktree_zero_copy_optional_branch(
            repo_path,
            target_path,
            new_branch,
            start_point,
            detach,
            &cleaned_sparse,
            cow_caches,
            zero_copy_devmap,
            gate,
        )?;
        let mut created = WorktreeCreated {
            path: sparse.path,
            cache_error: sparse.cache_error,
            low_disk: None,
            disk_unknown: false,
            hook_error: None,
            hook_notice: None,
            devmap: sparse.devmap,
        };
        if run_hooks {
            let repo = validate_repo(repo_path)?;
            let branch_name = new_branch.unwrap_or("");
            match run_repo_hooks(
                &repo,
                Path::new(&created.path),
                "post_create",
                &[
                    ("GITPULSE_BRANCH", branch_name),
                    ("GITPULSE_WORKTREE_PATH", &created.path),
                ],
            ) {
                Ok(outcome) => created.hook_notice = outcome.skipped,
                Err(e) => created.hook_error = Some(e.into_message()),
            }
        }
        return Ok(created);
    }

    let selection = crate::engine::cow_clone::CacheSelection::default();
    add_worktree_with_devmap(
        repo_path,
        target_path,
        new_branch,
        start_point,
        detach,
        cow_caches.then_some(&selection),
        run_hooks,
        devmap,
    )
}

/// [`add_worktree_extended`] with the cache copy's selection (bounds and
/// free-space reader) given, or `None` for no caches. It seeds no DevMap store:
/// use [`add_worktree_with_devmap`] to ask for one.
pub fn add_worktree_cached(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    caches: Option<&crate::engine::cow_clone::CacheSelection>,
    run_hooks: bool,
) -> Result<WorktreeCreated, CommandError> {
    add_worktree_with_devmap(
        repo_path,
        target_path,
        new_branch,
        start_point,
        detach,
        caches,
        run_hooks,
        DevmapMode::None,
    )
}

/// [`add_worktree_cached`] that also seeds the new worktree's DevMap store as
/// `devmap` says, in parallel with the cache copy, and reports what that did in
/// [`WorktreeCreated::devmap`]: a seeding that failed is `failed` with its
/// error, never a silent success.
#[allow(clippy::too_many_arguments)]
pub fn add_worktree_with_devmap(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    caches: Option<&crate::engine::cow_clone::CacheSelection>,
    run_hooks: bool,
    devmap: DevmapMode,
) -> Result<WorktreeCreated, CommandError> {
    let path = add_worktree(repo_path, target_path, new_branch, start_point, detach)?;
    let mut created = WorktreeCreated {
        path,
        cache_error: None,
        low_disk: None,
        disk_unknown: false,
        hook_error: None,
        hook_notice: None,
        devmap: DevmapOutcome::NotRequested,
    };
    let target = Path::new(&created.path);
    let anchor = resolve_worktree_family(repo_path, &created.path)
        .ok()
        .map(|f| f.anchor)
        .unwrap_or_else(|| Path::new(repo_path).to_path_buf());
    let free_space = caches.map_or(
        crate::sysload::free_bytes as crate::sysload::FreeSpace,
        |s| s.free_space,
    );
    let low_space = devmap == DevmapMode::Copy
        && matches!(
            crate::engine::cow_clone::read_free(free_space, target),
            crate::sysload::Reading::Value(left) if left < crate::sysload::LOW_DISK_FLOOR
        );
    let provision = || match devmap {
        DevmapMode::None => DevmapOutcome::NotRequested,
        DevmapMode::Stacked => DevmapOutcome::from_provision(
            crate::codeintel::provision_worktree_devmap_stacked(&anchor, target),
        ),
        DevmapMode::ZeroCopy => DevmapOutcome::from_provision(
            crate::codeintel::provision_worktree_devmap_zero_copy(&anchor, target),
        ),
        DevmapMode::Copy if low_space => DevmapOutcome::Skipped {
            reason: "skipped local devmap clone under low disk".to_string(),
        },
        DevmapMode::Copy => DevmapOutcome::from_provision(
            crate::codeintel::provision_worktree_devmap(&anchor, target),
        ),
    };
    if let Some(selection) = caches {
        let repo = validate_repo(repo_path)?;
        let (cache_outcome, devmap_outcome) = std::thread::scope(|scope| {
            let cache_task = scope.spawn(|| clone_ignored_caches(&repo, target, selection));
            let devmap_task = scope.spawn(provision);
            let devmap_outcome = devmap_task
                .join()
                .unwrap_or_else(|_| DevmapOutcome::Failed {
                    error: "devmap provisioning worker panicked".to_string(),
                });
            let cache_outcome = cache_task
                .join()
                .unwrap_or_else(|_| Err(CommandError::plain("cache clone worker panicked")));
            (cache_outcome, devmap_outcome)
        });
        created.devmap = devmap_outcome;
        match cache_outcome {
            Ok(report) => {
                created.cache_error = report.error();
                created.low_disk = report.low_disk;
                created.disk_unknown = report.disk_unknown;
            }
            Err(error) => created.cache_error = Some(error.into_message()),
        }
    } else {
        created.devmap = provision();
        // No caches were asked for, but `.worktreeinclude` is the repository's
        // own list of local files, which every worktree gets.
        let repo = validate_repo(repo_path)?;
        let report = crate::engine::cow_clone::copy_worktree_include(
            &repo,
            target,
            &crate::engine::cow_clone::CacheSelection::default(),
        );
        created.cache_error = report.error();
        created.low_disk = report.low_disk;
    }
    if run_hooks {
        let repo = validate_repo(repo_path)?;
        let branch_name = new_branch.unwrap_or("");
        match run_repo_hooks(
            &repo,
            target,
            "post_create",
            &[
                ("GITPULSE_BRANCH", branch_name),
                ("GITPULSE_WORKTREE_PATH", &created.path),
            ],
        ) {
            Ok(outcome) => created.hook_notice = outcome.skipped,
            Err(e) => created.hook_error = Some(e.into_message()),
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
    /// The commit the target ref points at after this call, when the merge
    /// was computed in the object database. Absent for the in-checkout fallback.
    #[serde(default)]
    pub candidate_oid: Option<String>,
    /// The main checkout's files were moved to that commit.
    #[serde(default)]
    pub checkout_advanced: bool,
    /// This git has no `merge-tree` write, so the lane was merged in the checkout.
    #[serde(default)]
    pub checkout_fallback: bool,
    /// Why the checkout was left where it was, or that the fallback ran.
    /// Shown to the person. Contains `checkout not advanced` when the ref
    /// moved and the files did not.
    #[serde(default)]
    pub checkout_note: Option<String>,
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
        /// The merge-tree tree, when the conflict was found in the object
        /// database and no worktree was touched. Absent for the in-checkout path.
        #[serde(default)]
        candidate: Option<String>,
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
    /// The candidate commit was built and rejected before the target moved.
    CandidateFailed { message: String, candidate: String },
    /// Nothing was merged or moved because a checkout with the target, or a
    /// hook the object-database commit would skip, makes it unsafe. `path`
    /// names the checkout when one is the cause. The wave holds the task.
    Held {
        reason: crate::engine::merge_tree::HeldReason,
        message: String,
        path: Option<String>,
    },
    /// The wave needs `merge-tree --write-tree` (git 2.38+) and this git
    /// lacks it. Nothing was merged.
    WriteTreeUnavailable { message: String },
}

impl MergeTeardownError {
    pub fn message(&self) -> &str {
        match self {
            Self::Conflict { message, .. }
            | Self::Moved { message, .. }
            | Self::BaseMismatch { message, .. }
            | Self::SquashParked { message }
            | Self::Refused { message }
            | Self::CandidateFailed { message, .. }
            | Self::Held { message, .. }
            | Self::WriteTreeUnavailable { message } => message,
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

/// This error is its own wire type (`{code, message, ...}`), never an
/// `IpcError`, so a kind has nowhere to go: the text is what it carries.
impl From<CommandError> for MergeTeardownError {
    fn from(error: CommandError) -> Self {
        Self::Refused {
            message: error.into_message(),
        }
    }
}

/// Written into a lane's private Git directory when it is provisioned: the
/// branch it was cut from and that branch's tip then, as `<branch>\n<oid>\n`.
pub const BASE_RECORD: &str = "gitpulse-base";

/// Records where `worktree` was cut from, next to the other per-worktree
/// state, never in its files where `git status` would show it.
pub fn write_base_record(worktree: &Path, branch: &str, oid: &str) -> Result<(), CommandError> {
    validate_ref_name(branch)?;
    if !is_full_oid(oid) {
        return Err(
            format!("{oid} is not a full commit id, so it is not recorded as a base").into(),
        );
    }
    let dir = resolve_git_dir(worktree)?;
    std::fs::write(dir.join(BASE_RECORD), format!("{branch}\n{oid}\n"))
        .map_err(|e| {
            format!(
                "could not write {BASE_RECORD} for {}: {e}",
                worktree.display()
            )
        })
        .map_err(CommandError::from)
}

/// The `(branch, oid)` [`write_base_record`] recorded. `None` when there is
/// no record (a lane made before it existed, or by the Worktrees panel). A
/// record that is there and cannot be read is an error, not "no record": a
/// check that could not run must not pass as one that did.
fn read_base_record(worktree: &Path) -> Result<Option<(String, String)>, CommandError> {
    let path = resolve_git_dir(worktree)?.join(BASE_RECORD);
    let raw = match std::fs::read(&path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("could not read {}: {e}", path.display()).into()),
    };
    let corrupt = |why: &str| format!("{} is not a branch and a commit id: {why}", path.display());
    if raw.len() > 4096 {
        return Err(corrupt("it is far larger than it should be").into());
    }
    let text = String::from_utf8(raw).map_err(|_| corrupt("it is not text"))?;
    let mut lines = text.lines().map(str::trim);
    let (Some(branch), Some(oid), None) = (lines.next(), lines.next(), lines.next()) else {
        return Err(corrupt("it does not have exactly two lines").into());
    };
    validate_ref_name(branch).map_err(|e| corrupt(e.message()))?;
    // A full id only: an abbreviation is whatever git resolves it to today.
    if !is_full_oid(oid) {
        return Err(corrupt("the commit id is not a full 40 or 64 character hex id").into());
    }
    Ok(Some((branch.to_string(), oid.to_ascii_lowercase())))
}

/// How many times the removal of a merged worktree is tried. A scanner or an
/// indexer holding a file open (Windows) fails the first try and lets go.
const MAX_REMOVE_ATTEMPTS: usize = 3;
/// Waits before the second and third try.
const REMOVE_BACKOFF: [Duration; MAX_REMOVE_ATTEMPTS - 1] =
    [Duration::from_millis(100), Duration::from_millis(300)];

/// Merges a worktree branch into target_branch (defaulting to the repository's default branch),
/// tears down the worktree, and prunes the merged branch.
///
/// The merge is computed in the object database (`merge-tree`, then
/// `commit-tree` when a new commit is needed, then a compare-and-swap
/// `update-ref`). The main checkout is moved to that commit only when it is
/// clean and already on the target. Below git 2.38 the same call merges in
/// the checkout instead, through [`merge_teardown_argv`], and restores that
/// checkout when the merge fails. The wave passes `require_write_tree` and
/// refuses rather than taking that fallback.
///
/// `gate` is asked about each git command before it runs, with git's
/// arguments as they actually run (no leading `git`): the object-database
/// steps, or on the fallback the merge of *this worktree's* branch, the
/// squash commit, and in either case the forced worktree removal and the
/// branch deletion. The caller used to ask once, up front, about
/// `merge --ff-only <target>` — a command this never ran, and spelled without
/// the program, so it was judged as a program named `merge` — and the three
/// later mutations were not asked about at all. Restoring an in-checkout
/// merge that conflicted is asked about too (`merge --abort`, or
/// `reset --merge` for a squash). A refusal is honoured: nothing is run, and
/// the error is a `Conflict` with `rolled_back: false` carrying the gate's
/// reason, leaving the checkout parked for a person. A conflict found by
/// `merge-tree` never starts that merge, so there is nothing to roll back.
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
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_requiring(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        false,
        gate,
    )
}

/// [`merge_and_teardown_worktree`] for the wave, which refuses below git 2.38
/// instead of merging in the checkout. `require_write_tree` false is the panel.
pub fn merge_and_teardown_requiring(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    require_write_tree: bool,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_verified(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        require_write_tree,
        None,
        gate,
    )
}

/// [`merge_and_teardown_requiring`] with a candidate check. `verify`, when
/// set, sees a detached scratch of the candidate and may reject it before
/// the target ref moves. The wait is not held under the repository lock.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_and_teardown_verified(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    require_write_tree: bool,
    verify: Option<&mut crate::engine::merge_tree::CandidateVerifier<'_>>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_configured(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        crate::engine::merge_tree::IntegrateConfig {
            require_write_tree,
            write_tree: crate::engine::merge_tree::WriteTreeMode::Detect,
        },
        verify,
        gate,
        &mut std::thread::sleep,
        &mut delete_dir_tree,
    )
}

/// [`merge_and_teardown_worktree`] with the wait between removal attempts
/// passed in, so a test does not sleep through the backoff.
#[cfg(test)]
fn merge_and_teardown_with(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
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
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn merge_and_teardown_with_seams(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
    sleep: &mut dyn FnMut(Duration),
    delete_aside: &mut dyn FnMut(&Path) -> std::io::Result<()>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_configured(
        repo_path,
        worktree_path,
        target_branch,
        squash,
        expected_tip,
        crate::engine::merge_tree::IntegrateConfig::default(),
        None,
        gate,
        sleep,
        delete_aside,
    )
}

/// [`merge_and_teardown_with_seams`] with the integrator and an optional
/// candidate check. `verify` sees a detached scratch worktree of the candidate
/// commit and may reject it before the target ref moves.
#[allow(clippy::too_many_arguments)]
pub(crate) fn merge_and_teardown_configured(
    repo_path: &str,
    worktree_path: &str,
    target_branch: Option<&str>,
    squash: bool,
    expected_tip: Option<&str>,
    config: crate::engine::merge_tree::IntegrateConfig,
    verify: Option<&mut crate::engine::merge_tree::CandidateVerifier<'_>>,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
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
                    format!(
                        "The default branch could not be read: {reason}. Nothing was merged.",
                        reason = reason.message()
                    )
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
    if branch == target {
        return Err(format!("Cannot merge {branch} into itself. Nothing was merged.").into());
    }

    // The in-checkout fallback merges onto whatever the main checkout has
    // checked out. It used to run there regardless and then report `target`:
    // a main checkout on another branch took the merge, and the panel said
    // "main". The object-database path moves the target ref wherever the
    // checkout is, and only fast-forwards a clean checkout that is on it.
    let object_db = crate::engine::merge_tree::write_tree_available(config.write_tree);
    if !object_db && config.require_write_tree {
        return Err(MergeTeardownError::WriteTreeUnavailable {
            message: crate::engine::merge_tree::wave_unsupported_message(),
        });
    }
    let main_is_on_target = || -> Result<(), CommandError> {
        // A refused read is an error, not "a detached HEAD": naming the wrong
        // reason would send the user to fix the wrong thing.
        let current = crate::engine::git_reader::head_branch(&repo)?.unwrap_or_default();
        if current != target {
            return Err(format!(
                "The main checkout is on {}, not {target}. Switch it to {target} first; nothing was merged.",
                if current.is_empty() { "a detached HEAD".to_string() } else { current }
            )
            .into());
        }
        Ok(())
    };
    if !object_db {
        main_is_on_target()?;
    }

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

    // A seeded background index refresh may still be writing into the lane.
    // Held after the pre-merge hooks, which run in the lane and may themselves
    // run `devmap build`, `seed --sync-delta` or `dev verify`: holding the
    // index's writer lock across them would make those children wait on their
    // own parent. Held before the repository lock and the merge, the first
    // irreversible step, so a refusal leaves the lane unmerged and intact and a
    // refresh that needs the lock cannot deadlock with the wait.
    let _no_refresh =
        crate::devmap::live::quiesce_for_removal(&plain_path_text(&worktree.to_string_lossy()))?;

    // Steps 3-6 hold one lock: the checks that the worktree is clean and that
    // the branch is what will be merged, the merge, and the removal and branch
    // deletion that destroy the worktree. They used to run in separate scopes,
    // so anything written between the check and the removal was destroyed with
    // it. The lock fences GitPulse's own writers; an outside process takes no
    // lock, so each destructive step re-reads the state it is about to destroy
    // and refuses, keeping the worktree and branch, when it moved. A candidate
    // check drops the lock for its wait and takes it again before the ref moves.
    let repo_lock = repo_mutation_lock(&repo);
    let mut guard = repo_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);

    // The pre-merge hooks above may have switched the main checkout. The
    // object-database path does not merge into that checkout, so it does not
    // refuse here.
    if !object_db {
        main_is_on_target()?;
    }

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
                "The main checkout's unmerged files could not be checked: {e}. Nothing was merged.",
                e = e.message()
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
    if let Some(expected) = expected_tip.as_deref() {
        if expected != merged_tip {
            return Err(MergeTeardownError::Moved {
                message: format!(
                    "{branch} is at {merged_tip}, not the {expected} that was checked. \
                     It moved after the check, so nothing was merged and the worktree was kept."
                ),
                branch: branch.clone(),
                expected: expected.to_string(),
                actual: merged_tip,
            });
        }
    }

    // The target the merge lands on. An unborn one has no tip to merge onto.
    let target_ref = format!("refs/heads/{target}");
    let before_oid = git_text(&repo, &["rev-parse", "--verify", target_ref.as_str()])
        .map_err(|e| {
            e.wrap(|e| format!("{target} has no commits to merge onto ({e}); nothing was merged."))
        })?
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

    // 4. Integrate the lane. The object database is the owner when this git
    // can write a merge tree; the in-checkout merge is only the older-git
    // fallback, and it is the one that calls [`merge_teardown_argv`].
    let mut candidate_oid = None;
    let checkout_note: Option<String>;
    let checkout_advanced: bool;
    let mut checkout_fallback = false;
    if object_db {
        // No verifier: one locked integrate, the same commands the gate pins
        // name. A verifier waits outside the lock, and only for a tree the
        // caller's tip does not already cover. The in-checkout fallback stays
        // on the unverified path.
        if let Some(verify) = verify {
            let candidate = crate::engine::merge_tree::prepare(
                &repo,
                &branch,
                target,
                &before_oid,
                &merged_tip,
                squash,
                gate,
            )
            .map_err(map_integrate_failure)?;
            let covered = crate::engine::merge_tree::already_covered(
                &repo,
                &candidate,
                &before_oid,
                expected_tip.as_deref(),
            )
            .map_err(map_integrate_failure)?;
            if !covered {
                drop(guard);
                let verdict =
                    crate::engine::merge_tree::verify_in_scratch(&repo, &candidate, verify, gate);
                guard = repo_lock
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                verdict.map_err(map_integrate_failure)?;
                recheck_after_verify(&repo, &worktree, &branch, target, &before_oid, &merged_tip)?;
            }
            let touch = checkout_holders(&repo, target)?;
            match crate::engine::merge_tree::land(
                &repo,
                &branch,
                target,
                &before_oid,
                &candidate,
                &touch,
                gate,
            ) {
                Ok(landed) => {
                    candidate_oid = Some(landed.candidate_oid);
                    checkout_note = landed.checkout_note;
                    checkout_advanced = landed.checkout_advanced;
                }
                Err(failure) => return Err(map_integrate_failure(failure)),
            }
        } else {
            let touch = checkout_holders(&repo, target)?;
            match crate::engine::merge_tree::integrate(
                &repo,
                &branch,
                target,
                &before_oid,
                &merged_tip,
                squash,
                &touch,
                None,
                gate,
            ) {
                Ok(landed) => {
                    candidate_oid = Some(landed.candidate_oid);
                    checkout_note = landed.checkout_note;
                    checkout_advanced = landed.checkout_advanced;
                }
                Err(crate::engine::merge_tree::IntegrateFailure::Unsupported { .. })
                    if !config.require_write_tree =>
                {
                    main_is_on_target()?;
                    in_checkout_merge(gate, &repo, &branch, target, &merged_tip, squash)?;
                    checkout_fallback = true;
                    checkout_advanced = true;
                    checkout_note = Some(crate::engine::merge_tree::fallback_note());
                }
                // The hooks the object-database commit would skip run in the
                // in-checkout merge, as they always did.
                Err(crate::engine::merge_tree::IntegrateFailure::CommitHooks { hooks })
                    if !config.require_write_tree =>
                {
                    main_is_on_target()?;
                    in_checkout_merge(gate, &repo, &branch, target, &merged_tip, squash)?;
                    checkout_fallback = true;
                    checkout_advanced = true;
                    checkout_note = Some(crate::engine::merge_tree::hooks_fallback_note(&hooks));
                }
                Err(failure) => return Err(map_integrate_failure(failure)),
            }
        }
    } else {
        in_checkout_merge(gate, &repo, &branch, target, &merged_tip, squash)?;
        checkout_fallback = true;
        checkout_advanced = true;
        checkout_note = Some(crate::engine::merge_tree::fallback_note());
    }

    // From here the merge has landed. Nothing below returns `Err`: what it
    // cannot finish is reported on the result.
    let (after_oid, after_error) =
        match git_text(&repo, &["rev-parse", "--verify", target_ref.as_str()]) {
            Ok(tip) => (Some(tip.trim().to_string()), None),
            Err(e) => (
                None,
                Some(format!(
                    "the new tip of {target} could not be read ({}), so the worktree and the branch were kept",
                    e.message()
                )),
            ),
        };

    // What the destructive steps below may destroy is what was merged.
    let unchanged_since_merge = |repo: &Path, worktree_present: bool| -> Result<(), CommandError> {
        let kept = format!(
            "The merge of {branch} into {target} is done; the worktree and the branch were kept"
        );
        if worktree_present {
            let (changed, _) = changed_paths(&worktree_text).map_err(|e| {
                e.wrap(|e| format!("{kept}, because the worktree could not be re-checked: {e}"))
            })?;
            if !changed.is_empty() {
                return Err(format!(
                    "{kept}: {} file change(s) appeared in the worktree after the merge was checked.",
                    changed.len()
                )
                .into());
            }
        }
        let tip = git_text(repo, &["rev-parse", "--verify", branch_ref.as_str()]).map_err(|e| {
            e.wrap(|e| format!("{kept}, because the branch could not be re-read: {e}"))
        })?;
        if tip.trim() != merged_tip {
            return Err(format!(
                "{kept}: {branch} moved from {merged_tip} to {} after the merge was checked.",
                tip.trim()
            )
            .into());
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
        Some(kept) => Err(CommandError::from(kept)),
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
        Err(e) => (false, Some(e.into_message()), None),
    };

    // 6. Delete merged branch. A branch checked out in a worktree that is
    // still there cannot be deleted, and is not wanted deleted.
    // `branch -d` judges HEAD. The object-database path can leave HEAD on
    // another branch, or detached at the previous commit, while
    // refs/heads/<target> already contains this branch. Forcing the delete
    // is then the same check `-d` would have made against the target.
    let force_delete = squash
        || matches!(
            (
                commit_is_ancestor(&repo, &merged_tip, "HEAD"),
                commit_is_ancestor(&repo, &merged_tip, &format!("refs/heads/{target}")),
            ),
            (Ok(false), Ok(true))
        );
    let delete = [
        "branch",
        if force_delete { "-D" } else { "-d" },
        branch.as_str(),
    ];
    let branch_result = if worktree_removed {
        gate(&delete)
            .and_then(|()| unchanged_since_merge(&repo, false))
            .and_then(|()| git_text(&repo, &delete).map(drop))
    } else {
        Err(CommandError::from(format!(
            "{branch} was kept because its worktree is still there"
        )))
    };
    let (branch_deleted, branch_error) = match branch_result {
        Ok(()) => (true, None),
        Err(e) => (false, Some(e.into_message())),
    };
    drop(guard);

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
        Err(e) => hook_error = Some(e.into_message()),
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
        candidate_oid,
        checkout_advanced,
        checkout_fallback,
        checkout_note,
    })
}

/// The repository can change while a candidate is verified, because that wait
/// does not hold the mutation lock. Land only when the tips and the lane are
/// still what the candidate was built from.
fn recheck_after_verify(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    target: &str,
    before_oid: &str,
    merged_tip: &str,
) -> Result<(), MergeTeardownError> {
    if let Some(kind) = crate::engine::repo_op::detect_kind(repo)? {
        return Err(MergeTeardownError::Refused {
            message: format!(
                "The main checkout picked up a {} while the candidate was being verified, so nothing was merged.",
                kind.label()
            ),
        });
    }
    let target_now = git_text(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{target}")],
    )?
    .trim()
    .to_string();
    if target_now != before_oid {
        return Err(MergeTeardownError::Moved {
            message: format!(
                "{target} moved from {before_oid} to {target_now} while the candidate was being verified, so nothing was merged."
            ),
            branch: branch.to_string(),
            expected: before_oid.to_string(),
            actual: target_now,
        });
    }
    let branch_now = git_text(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
    )?
    .trim()
    .to_string();
    if branch_now != merged_tip {
        return Err(MergeTeardownError::Moved {
            message: format!(
                "{branch} moved from {merged_tip} to {branch_now} while the candidate was being verified, so nothing was merged."
            ),
            branch: branch.to_string(),
            expected: merged_tip.to_string(),
            actual: branch_now,
        });
    }
    let text = worktree
        .to_str()
        .ok_or_else(|| MergeTeardownError::Refused {
            message:
                "The worktree path is not UTF-8, so it could not be re-checked. Nothing was merged."
                    .into(),
        })?;
    let (changed, _) = changed_paths(text)?;
    if !changed.is_empty() {
        return Err(MergeTeardownError::Refused {
            message: format!(
                "The worktree gained {} uncommitted change(s) while the candidate was being verified, so nothing was merged.",
                changed.len()
            ),
        });
    }
    Ok(())
}

/// Whether `ancestor` is contained in `descendant` (`merge-base --is-ancestor`).
/// Exit 0 and 1 are the answer. Any other exit is a failed read.
fn commit_is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool, CommandError> {
    let run = crate::engine::git_cli::git_captured(
        repo,
        &["merge-base", "--is-ancestor", ancestor, descendant],
    )?;
    match run.status_code {
        0 => Ok(true),
        1 => Ok(false),
        code => Err(format!(
            "Could not tell whether {descendant} contains {ancestor} (git exit {code}): {}",
            String::from_utf8_lossy(&run.stderr).trim()
        )
        .into()),
    }
}

/// Every checkout that has `target` checked out (the main checkout or a linked
/// worktree), as the object-database path needs them, under the lock. Moving
/// the target ref moves each one's HEAD, so each is read for what would stop
/// it being moved: uncommitted files, a held `index.lock`, or being unreadable.
fn checkout_holders(
    repo: &Path,
    target: &str,
) -> Result<crate::engine::merge_tree::CheckoutTouch, MergeTeardownError> {
    use crate::engine::merge_tree::{CheckoutHolder, CheckoutTouch};
    let entries = read_worktree_entries(repo).map_err(|e| MergeTeardownError::Refused {
        message: format!(
            "The worktrees that have {target} checked out could not be listed ({}), so nothing was merged.",
            e.message()
        ),
    })?;
    let mut holders = Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.branch.as_deref() == Some(target))
    {
        let path = PathBuf::from(&entry.path);
        let problem = checkout_problem(&path, entry.is_prunable);
        holders.push(CheckoutHolder { path, problem });
    }
    Ok(CheckoutTouch { holders })
}

fn checkout_problem(
    path: &Path,
    prunable: bool,
) -> Option<crate::engine::merge_tree::HolderProblem> {
    use crate::engine::merge_tree::HolderProblem;
    if prunable || !path.is_dir() {
        return Some(HolderProblem::Unreadable(
            "its directory is missing; `git worktree prune` removes the stale entry".into(),
        ));
    }
    match resolve_git_dir(path) {
        Ok(git_dir) => {
            if std::fs::symlink_metadata(git_dir.join("index.lock")).is_ok() {
                return Some(HolderProblem::IndexLocked);
            }
        }
        Err(error) => return Some(HolderProblem::Unreadable(error.into_message())),
    }
    let Some(text) = path.to_str() else {
        return Some(HolderProblem::Unreadable("its path is not UTF-8".into()));
    };
    match user_changed_paths(text) {
        Ok((paths, _)) if paths.is_empty() => None,
        Ok(_) => Some(HolderProblem::Dirty),
        Err(error) => Some(HolderProblem::Unreadable(error.into_message())),
    }
}

fn map_integrate_failure(
    failure: crate::engine::merge_tree::IntegrateFailure,
) -> MergeTeardownError {
    use crate::engine::merge_tree::IntegrateFailure;
    match failure {
        IntegrateFailure::Conflict {
            message,
            paths,
            total,
            candidate,
        } => MergeTeardownError::Conflict {
            message,
            paths,
            total,
            rolled_back: true,
            rollback_error: None,
            candidate: Some(candidate),
        },
        IntegrateFailure::CandidateFailed { message, candidate } => {
            MergeTeardownError::CandidateFailed { message, candidate }
        }
        IntegrateFailure::Refused { message } => MergeTeardownError::Refused { message },
        IntegrateFailure::Unsupported { message } => MergeTeardownError::WriteTreeUnavailable {
            message: format!(
                "{} (git said: {message})",
                crate::engine::merge_tree::wave_unsupported_message()
            ),
        },
        IntegrateFailure::Held {
            reason,
            message,
            path,
        } => MergeTeardownError::Held {
            reason,
            message,
            path,
        },
        IntegrateFailure::CommitHooks { hooks } => MergeTeardownError::Held {
            reason: crate::engine::merge_tree::HeldReason::CommitHooks,
            message: crate::engine::merge_tree::hooks_refusal_message(&hooks),
            path: None,
        },
    }
}

/// The merge that runs in the main checkout when `merge-tree` cannot write a
/// tree. Both arms go through [`merge_teardown_argv`] for the first command.
fn in_checkout_merge(
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
    repo: &Path,
    branch: &str,
    target: &str,
    merged_tip: &str,
    squash: bool,
) -> Result<(), MergeTeardownError> {
    if squash {
        // Nothing staged means the branch's changes are already on the
        // target and there is nothing to commit — not a failure. A failed
        // commit fails closed before anything is torn down: it used to be
        // discarded, and the next steps removed the worktree and
        // force-deleted the branch whose work was never committed.
        GitWriter::squash_merge_locked(repo, branch, gate).map_err(|failure| match failure {
            SquashFailure::Merge(e) => MergeTeardownError::Refused {
                message: e.into_message(),
            },
            SquashFailure::Conflict(e) => {
                failed_merge_error(gate, repo, branch, target, merged_tip, e.into_message())
            }
            // Staged, not rolled back: `git reset --merge` is the user's call.
            SquashFailure::Commit(e) => MergeTeardownError::SquashParked {
                message: format!(
                    "{} The worktree and the branch {branch} were kept on {target}.",
                    e.message()
                ),
            },
        })?;
    } else {
        let fast_forward = merge_teardown_argv(branch, false);
        let fast_forward: Vec<&str> = fast_forward.iter().map(String::as_str).collect();
        gate(&fast_forward)?;
        if let Err(ff_err) = git_text(repo, &fast_forward) {
            // The same two guards the squash runs, before the commit a merge
            // commit is.
            GitWriter::require_commit_identity(repo)?;
            let commit_msg = format!("Merge branch '{branch}' into {target}");
            let merge = ["merge", "--no-edit", branch, "-m", commit_msg.as_str()];
            gate(&merge)?;
            if let Err(e) = git_text(repo, &merge) {
                return Err(failed_merge_error(
                    gate,
                    repo,
                    branch,
                    target,
                    merged_tip,
                    format!(
                        "Merge failed (ff error: {}): {}",
                        ff_err.message(),
                        e.message()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// What a merge that failed after it started leaves the caller: a conflict,
/// with the checkout restored, or, when git failed for a reason that changed
/// nothing, a plain refusal. Runs under the repository lock.
fn failed_merge_error(
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
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
        Err(e) => (false, Some(e.into_message())),
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
        .map(|e| {
            format!(
                " The conflicted files could not be listed: {}.",
                e.message()
            )
        })
        .unwrap_or_default();
    MergeTeardownError::Conflict {
        message: format!("Merging {branch} into {target} {what}. {outcome}{listing_note}"),
        paths,
        total,
        rolled_back,
        rollback_error,
        candidate: None,
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
fn still_listed(repo: &Path, worktree_text: &str) -> Result<bool, CommandError> {
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
fn remove_own_worktree_record(repo: &Path, worktree_text: &str) -> Result<(), CommandError> {
    let Some(record) = find_own_worktree_record(repo, worktree_text)? else {
        return Ok(());
    };
    if record.join("locked").exists() {
        return Err(format!("its record {} is locked, so it was left", record.display()).into());
    }
    std::fs::remove_dir_all(&record)
        .map_err(|e| format!("could not remove its record {}: {e}", record.display()).into())
}

/// The directories of `<common>/worktrees` (one record per linked worktree)
/// with the path each one's `gitdir` file points at, which is `<worktree>/.git`.
fn worktree_records(repo: &Path) -> Result<Vec<(PathBuf, PathBuf)>, CommandError> {
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
                return Err(
                    format!("could not read {}: {e}", record.join("gitdir").display()).into(),
                )
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
fn find_own_worktree_record(
    repo: &Path,
    worktree_text: &str,
) -> Result<Option<PathBuf>, CommandError> {
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
) -> Result<Option<TeardownAside>, CommandError> {
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
    remove_record: &mut dyn FnMut(&Path, &str) -> Result<(), CommandError>,
    rename_back: &mut dyn FnMut(&Path, &Path) -> std::io::Result<()>,
) -> Result<Option<TeardownAside>, CommandError> {
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
            return Err(put_back(format!("could not mark the aside as ours: {e}")).into());
        }
    }
    if let Err(e) = remove_record(repo, target_text) {
        return Err(e.wrap(|why| put_back(why.to_string())));
    }
    match still_listed(repo, target_text) {
        Ok(false) => Ok(Some(held)),
        Ok(true) => Err(put_back(format!("git still lists {target_text}")).into()),
        Err(e) => Err(e.wrap(|why| put_back(why.to_string()))),
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
    fallback: &mut dyn FnMut() -> Result<(), CommandError>,
) -> Result<Option<TeardownAside>, CommandError> {
    match try_rename_aside(repo, target_text, force)? {
        Some(aside) => Ok(Some(aside)),
        None => fallback().map(|()| None),
    }
}

/// The production deleter for [`finish_teardown`]. A named function, not
/// `std::fs::remove_dir_all` itself, which does not satisfy the seam's
/// higher-ranked `&Path` signature.
fn delete_dir_tree(path: &Path) -> std::io::Result<()> {
    delete_dir_tree_with(path, &|path| std::fs::remove_dir_all(path))
}

/// How many times a recursive delete is retried after NotFound from inside
/// its walk before the NotFound is reported.
const VANISHED_WALK_RETRIES: usize = 8;

/// Whether `path` is confirmed absent (NotFound, not any other error and not
/// a symlink's target). The one check that lets a NotFound from a delete count
/// as "already removed".
fn path_is_gone(path: &Path) -> bool {
    matches!(std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound)
}

/// Deletes a tree with `remove_once`, treating NotFound as the goal state. A
/// concurrent sweep can delete the tree, or children of it, while this walk is
/// reading it, so NotFound can come from the top or from inside the walk: if
/// the tree is gone it is done, and if it is not the walk is run again over
/// what is left. Every other error is returned as it came, and a NotFound
/// that persists past [`VANISHED_WALK_RETRIES`] with the tree still there is
/// returned too.
fn delete_dir_tree_with(
    path: &Path,
    remove_once: &dyn Fn(&Path) -> std::io::Result<()>,
) -> std::io::Result<()> {
    let mut attempt = 0;
    loop {
        match remove_once(path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if path_is_gone(path) {
                    return Ok(());
                }
                attempt += 1;
                if attempt > VANISHED_WALK_RETRIES {
                    return Err(e);
                }
            }
            other => return other,
        }
    }
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
    sweep_teardown_asides_from(
        repo,
        &|holding| {
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
        },
        &delete_dir_tree,
    )
}

/// A listing of the holding directory: `(name, path)` per entry, or the error
/// reading that entry, or the error listing it at all.
type EntryListing = std::io::Result<Vec<std::io::Result<(String, PathBuf)>>>;

/// [`sweep_teardown_asides`] with the listing of the holding directory and the
/// deleter passed in, so a test can hand it an entry that failed to read or an
/// aside that another sweep deletes first.
fn sweep_teardown_asides_from(
    repo: &Path,
    list_entries: &dyn Fn(&Path) -> EntryListing,
    delete_tree: &dyn Fn(&Path) -> std::io::Result<()>,
) -> SweepReport {
    let mut report = SweepReport::default();
    let holding = match resolve_git_common_dir(repo) {
        Ok(common) => common.join(TEARDOWN_DIR),
        Err(e) => {
            report.problems.push(format!(
                "leftover teardown directories were not checked: {e}",
                e = e.message()
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
                    "the worktree records could not be read, so no leftover was deleted this time: {e}", e = e.message()
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
        match delete_tree(&target) {
            Ok(()) => {
                if let Some(marker) = &marker {
                    let _ = std::fs::remove_file(marker);
                }
                report.removed += 1;
            }
            // Gone already (another sweep deleted it between the listing and
            // this delete): the goal state, and that sweep reports it. Only
            // when the aside is confirmed gone: a NotFound with the tree still
            // there is a failure, and the marker is what finds a sibling aside.
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && path_is_gone(&target) => {
                if let Some(marker) = &marker {
                    let _ = std::fs::remove_file(marker);
                }
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
) -> Result<(), CommandError> {
    let mut last = CommandError::plain(String::new());
    for attempt in 0..MAX_REMOVE_ATTEMPTS {
        if attempt > 0 {
            sleep(REMOVE_BACKOFF[attempt - 1]);
        }
        match remove_worktree_locked(repo, worktree_text, true) {
            Ok(()) => {
                return if still_listed(repo, worktree_text)? {
                    Err(format!("git removed {worktree_text} but still lists it").into())
                } else {
                    Ok(())
                };
            }
            Err(e) => last = e,
        }
        if !Path::new(worktree_text).exists() {
            remove_own_worktree_record(repo, worktree_text).map_err(|e| {
                last.clone()
                    .wrap(|last| format!("{last}; finishing the removal failed: {}", e.message()))
            })?;
            return if still_listed(repo, worktree_text)? {
                Err(last.wrap(|last| format!("{last}; git still lists {worktree_text}")))
            } else {
                Ok(())
            };
        }
    }
    Err(last.wrap(|last| {
        format!("The worktree could not be removed after {MAX_REMOVE_ATTEMPTS} attempts: {last}")
    }))
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
) -> Result<WorktreeRemoval, CommandError> {
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
) -> Result<WorktreeRemoval, CommandError> {
    let repo = validate_repo(repo_path)?;
    // A path the removal refuses outright is refused before anything is
    // quiesced: waiting on the primary checkout's index, then taking its slot
    // and lock, would only precede the same refusal. Both checks read nothing
    // that needs the repository lock, and the lock's own re-check stays.
    validate_target_path(target_path)?;
    let target_identity = lane_identity(Path::new(&plain_path_text(target_path)));
    if read_worktree_entries(&repo)?
        .first()
        .is_some_and(|primary| lane_identity(Path::new(&primary.path)) == target_identity)
    {
        return Err("Cannot remove the repository's primary checkout"
            .to_string()
            .into());
    }
    // A seeded background index refresh may still be writing into the target;
    // hold its writer slot, bounded, before anything is renamed or deleted.
    let _no_refresh = crate::devmap::live::quiesce_for_removal(&plain_path_text(target_path))?;
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
            let _ = crate::engine::agent_bus::release_lanes_matching(
                repo_path,
                Some(Path::new(&plain_path_text(target_path))),
                None,
            );
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
    let _ = crate::engine::agent_bus::release_lanes_matching(
        repo_path,
        Some(Path::new(&plain_path_text(target_path))),
        None,
    );
    Ok(WorktreeRemoval {
        cleanup_error: finish_teardown(aside, delete_aside),
        sweep_problems,
    })
}

/// Whether `target_path` is a worktree whose directory is already gone, and
/// if so removes its record (and nothing else), under the lock the caller
/// holds. `false` for a path that exists, or that no record names, which the
/// removal then handles as it always did.
fn finish_vanished_worktree(repo: &Path, target_path: &str) -> Result<bool, CommandError> {
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
        return Err(format!("git still lists {text} after its record was removed").into());
    }
    Ok(true)
}

/// [`remove_worktree`] for a caller that already holds `repo`'s mutation lock
/// (the merge-and-teardown span, which checks and removes under one lock).
fn remove_worktree_locked(repo: &Path, target_path: &str, force: bool) -> Result<(), CommandError> {
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
) -> Result<(), CommandError> {
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
pub fn unlock_worktree(repo_path: &str, target_path: &str) -> Result<(), CommandError> {
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
pub fn prune_worktree(repo_path: &str) -> Result<(), CommandError> {
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
    add_worktree_argv_configured(target_path, new_branch, start_point, detach, false)
}

/// Exact `git worktree add` argv for both full checkouts and `--no-checkout`
/// sparse-cone checkouts, so the policy gate judges the exact line executed.
pub fn add_worktree_argv_configured(
    target_path: &str,
    new_branch: Option<&str>,
    start_point: Option<&str>,
    detach: bool,
    sparse_no_checkout: bool,
) -> Vec<String> {
    let mut argv: Vec<String> = vec!["git".into(), "worktree".into(), "add".into()];
    if sparse_no_checkout {
        argv.push("--no-checkout".into());
    }
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

/// `git worktree list --porcelain -z`, the exact line [`read_worktree_entries`]
/// runs, so a caller can judge it before reading the listing.
pub fn worktree_list_porcelain_argv() -> Vec<String> {
    vec![
        "git".into(),
        "worktree".into(),
        "list".into(),
        "--porcelain".into(),
        "-z".into(),
    ]
}

/// Sanitizes a branch name for safe directory naming (replaces `/`, `\`, `:`, whitespace, etc. with `-`).
pub fn sanitize_branch_name(branch: &str) -> String {
    let sanitized: String = branch
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '.' || c == '_' || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect();
    let trimmed = sanitized.trim_matches(|c| c == '-' || c == '.');
    if trimmed.is_empty() {
        "worktree".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Renders a branch template safely, substituting variables:
/// `repo`, `branch`, `branch | sanitize`, `task_id`, `agent_id`, `short_sha`.
pub fn render_branch_template(
    template: &str,
    repo_name: &str,
    branch: &str,
    task_id: Option<&str>,
    agent_id: Option<&str>,
    short_sha: Option<&str>,
) -> Result<String, CommandError> {
    let sanitized_branch = sanitize_branch_name(branch);
    let mut rendered = template.to_string();
    rendered = rendered.replace("{{ repo }}", repo_name);
    rendered = rendered.replace("{{repo}}", repo_name);
    rendered = rendered.replace("{{ branch | sanitize }}", &sanitized_branch);
    rendered = rendered.replace("{{branch | sanitize}}", &sanitized_branch);
    rendered = rendered.replace("{{ branch }}", branch);
    rendered = rendered.replace("{{branch}}", branch);
    rendered = rendered.replace("{{ task_id }}", task_id.unwrap_or(""));
    rendered = rendered.replace("{{task_id}}", task_id.unwrap_or(""));
    rendered = rendered.replace("{{ agent_id }}", agent_id.unwrap_or(""));
    rendered = rendered.replace("{{agent_id}}", agent_id.unwrap_or(""));
    rendered = rendered.replace("{{ short_sha }}", short_sha.unwrap_or(""));
    rendered = rendered.replace("{{short_sha}}", short_sha.unwrap_or(""));

    if rendered.contains("{{") || rendered.contains("}}") {
        return Err(CommandError::plain(format!(
            "template contains unrendered tags: {rendered}"
        )));
    }
    if rendered.contains('\0') {
        return Err(CommandError::plain("rendered template contains null byte"));
    }
    Ok(rendered)
}

/// Resolves a deterministic target worktree path for a branch in a repository.
pub fn resolve_worktree_path_for_branch(
    repo_path: &str,
    branch: &str,
    base_target: Option<&str>,
) -> Result<PathBuf, CommandError> {
    if let Some(target) = base_target {
        validate_target_path(target)?;
        refuse_gitpulse_lane_target(target)?;
        return Ok(PathBuf::from(target));
    }
    let repo = validate_repo(repo_path)?;
    let anchor = resolve_worktree_family(repo_path, repo_path)
        .ok()
        .and_then(|f| f.anchor.to_str().and_then(|s| validate_repo(s).ok()))
        .unwrap_or(repo);
    let repo_name = anchor
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo");
    let template = "../{{ repo }}.{{ branch | sanitize }}";
    let rendered = render_branch_template(template, repo_name, branch, None, None, None)?;
    let parent = anchor.parent().unwrap_or(&anchor);
    let target = parent.join(rendered.trim_start_matches("../"));
    if let Some(target_str) = target.to_str() {
        refuse_gitpulse_lane_target(target_str)?;
    }
    Ok(target)
}

/// Results of the Six-Point Branch Integration Verification Algorithm.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SixPointCheckResult {
    pub branch: String,
    pub target: String,
    pub same_commit: bool,
    pub is_ancestor: bool,
    pub no_added_changes: bool,
    pub trees_match: bool,
    pub merge_adds_nothing: bool,
    pub patch_id_match: bool,
    pub is_integrated: bool,
}

/// Verifies whether a branch is fully integrated into a target branch via 6 distinct checks:
/// 1. SameCommit: branch SHA == target SHA
/// 2. Ancestor: git merge-base --is-ancestor branch target
/// 3. NoAddedChanges: git diff target...branch is empty
/// 4. TreesMatch: branch tree == target tree
/// 5. MergeAddsNothing: quarantined `git merge-tree --write-tree` matches target tree (squash merges)
/// 6. PatchIdMatch: `git cherry` confirms every commit on branch has an equivalent patch-id in target (rebases/cherry-picks)
pub fn verify_branch_integration(
    repo: &Path,
    branch: &str,
    target: &str,
) -> Result<SixPointCheckResult, CommandError> {
    validate_ref_name(branch)?;
    validate_ref_name(target)?;

    let branch_sha =
        git_text(repo, &["rev-parse", "--verify", branch]).map(|s| s.trim().to_string())?;
    let target_sha =
        git_text(repo, &["rev-parse", "--verify", target]).map(|s| s.trim().to_string())?;

    // Check 1: SameCommit
    let same_commit = branch_sha == target_sha;

    // Check 2: Ancestor (merge-base --is-ancestor branch target)
    let is_ancestor = crate::engine::git_cli::git_captured(
        repo,
        &["merge-base", "--is-ancestor", &branch_sha, &target_sha],
    )
    .map(|run| run.status_code == 0)
    .unwrap_or(false);

    // Check 3: NoAddedChanges (git diff target...branch --quiet)
    let no_added_changes = crate::engine::git_cli::git_captured(
        repo,
        &["diff", &format!("{target_sha}...{branch_sha}"), "--quiet"],
    )
    .map(|run| run.status_code == 0)
    .unwrap_or(false);

    // Check 4: TreesMatch (rev-parse branch^{tree} == rev-parse target^{tree})
    let branch_tree = git_text(repo, &["rev-parse", &format!("{branch_sha}^{{tree}}")])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let target_tree = git_text(repo, &["rev-parse", &format!("{target_sha}^{{tree}}")])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    let trees_match = !branch_tree.is_empty() && branch_tree == target_tree;

    // Check 5: MergeAddsNothing (quarantined 3-way merge-tree --write-tree produces target_tree)
    let merge_adds_nothing = if same_commit || is_ancestor || trees_match {
        true
    } else if !target_tree.is_empty() {
        match crate::engine::object_quarantine::ObjectQuarantine::new(repo) {
            Ok(quarantine) => match quarantine.merge_tree(
                repo,
                &target_sha,
                &branch_sha,
                Instant::now() + crate::engine::git_cli::DEFAULT_TIMEOUT,
            ) {
                Ok(crate::engine::object_quarantine::MergeOutcome::Clean { tree }) => {
                    tree == target_tree
                }
                _ => false,
            },
            Err(_) => false,
        }
    } else {
        false
    };

    // Check 6: PatchIdMatch (`git cherry` confirms every non-ancestor commit has an equivalent patch-id in target)
    let patch_id_match = if same_commit || is_ancestor {
        true
    } else {
        match git_text(repo, &["cherry", &target_sha, &branch_sha]) {
            Ok(cherry) => {
                let mut saw_equivalent = false;
                let mut any_unmatched = false;
                for line in cherry.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    if line.starts_with("- ") {
                        saw_equivalent = true;
                    } else {
                        any_unmatched = true;
                    }
                }
                saw_equivalent && !any_unmatched
            }
            Err(_) => false,
        }
    };

    let is_integrated = same_commit
        || is_ancestor
        || no_added_changes
        || trees_match
        || merge_adds_nothing
        || patch_id_match;

    Ok(SixPointCheckResult {
        branch: branch.to_string(),
        target: target.to_string(),
        same_commit,
        is_ancestor,
        no_added_changes,
        trees_match,
        merge_adds_nothing,
        patch_id_match,
        is_integrated,
    })
}

/// Report of process reaping operation in a worktree directory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessReapReport {
    pub pids_examined: Vec<i32>,
    pub pids_terminated: Vec<i32>,
    pub pids_spared: Vec<i32>,
    pub dry_run: bool,
}

/// Reaps detached background processes working in `dir` through [`crate::procguard::signal`],
/// sparing interactive shells, live agent sessions, supervisor-owned launches, `memguard`,
/// and processes belonging to another user, while failing closed if the process table cannot be read.
pub fn reap_processes_in_dir(dir: &Path, dry_run: bool) -> Result<ProcessReapReport, CommandError> {
    let pids = crate::sysload::procscan::working_in(dir).map_err(CommandError::plain)?;

    // Only the unix arm of the loop terminates anything; elsewhere every pid is spared.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut terminated = Vec::new();
    let mut spared = Vec::new();

    #[cfg(unix)]
    let me_uid = unsafe { libc::getuid() };
    #[cfg(unix)]
    let start_of = |p: i32| crate::sysload::procscan::probe_one(p).map(|facts| facts.start_us);

    for &pid in &pids {
        #[cfg(unix)]
        {
            if pid <= 1 || pid == std::process::id() as i32 {
                spared.push(pid);
                continue;
            }
            let Some(facts) = crate::sysload::procscan::probe_one(pid) else {
                // Exited between `working_in` and `probe_one`.
                continue;
            };
            let base_name = facts.ident.name.trim_start_matches('-');
            let is_interactive_shell = matches!(
                base_name,
                "sh" | "bash" | "zsh" | "fish" | "nu" | "tcsh" | "csh" | "ksh" | "tmux" | "screen"
            );
            if facts.start_us == 0
                || facts.uid != me_uid
                || facts.ident.memguard
                || facts.ident.launch.is_some()
                || facts.ident.kind.is_some()
                || is_interactive_shell
            {
                spared.push(pid);
                continue;
            }

            if dry_run {
                terminated.push(pid);
                continue;
            }

            let sigterm_res = crate::procguard::signal::signal_process(
                pid,
                facts.start_us,
                &start_of,
                libc::SIGTERM,
                "worktree reap",
            );
            match sigterm_res {
                Ok(()) => {
                    let mut exited = false;
                    for _ in 0..10 {
                        std::thread::sleep(Duration::from_millis(50));
                        let still_same = crate::sysload::procscan::probe_one(pid)
                            .is_some_and(|now| now.start_us == facts.start_us);
                        if !still_same {
                            exited = true;
                            break;
                        }
                    }
                    if !exited {
                        let _ = crate::procguard::signal::signal_process(
                            pid,
                            facts.start_us,
                            &start_of,
                            libc::SIGKILL,
                            "worktree reap escalate",
                        );
                    }
                    terminated.push(pid);
                }
                Err(_) => {
                    if crate::sysload::procscan::probe_one(pid)
                        .is_some_and(|now| now.start_us == facts.start_us)
                    {
                        spared.push(pid);
                    }
                }
            }
        }
        #[cfg(not(unix))]
        {
            spared.push(pid);
        }
    }

    Ok(ProcessReapReport {
        pids_examined: pids,
        pids_terminated: terminated,
        pids_spared: spared,
        dry_run,
    })
}

/// Result of automated worktree merge pipeline execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorktreeMergeResult {
    pub ok: bool,
    pub source_branch: String,
    pub target_branch: String,
    pub squashed_commit: Option<String>,
    pub merged_sha: Option<String>,
    pub worktree_removed: bool,
    pub error: Option<String>,
}

/// Returns `true` when `rel_path` names a machine-generated runtime state file
/// written by GitPulse or DevMap itself (`.devcouncil/ledger.sqlite*`,
/// `.devcouncil/codeintel/**`, `.devmap/codeintel/**`), rather than a user or
/// agent source edit. User-authored files such as `.devcouncil/config.yaml`
/// return `false` so uncommitted edits to them remain protected.
pub fn is_runtime_state_artifact_path(rel_path: &str) -> bool {
    let normalized = rel_path.trim_matches('/');
    if matches!(
        normalized,
        ".devcouncil/ledger.sqlite"
            | ".devcouncil/ledger.sqlite-wal"
            | ".devcouncil/ledger.sqlite-shm"
            | ".devcouncil/ledger.sqlite-journal"
            | ".devmap/ledger.sqlite"
            | ".devmap/ledger.sqlite-wal"
            | ".devmap/ledger.sqlite-shm"
            | ".devmap/ledger.sqlite-journal"
    ) {
        return true;
    }
    normalized.starts_with(".devcouncil/codeintel/") || normalized.starts_with(".devmap/codeintel/")
}

/// Repo-relative uncommitted user/agent paths in `worktree_path`, excluding
/// GitPulse/DevMap's own runtime SQLite and codeintel artifacts. Fails closed
/// if the underlying status scan was capped (`truncated`) with no visible user
/// path, so a capped status sample is never mistaken for a clean checkout.
pub fn user_changed_paths(worktree_path: &str) -> Result<(Vec<String>, bool), CommandError> {
    let (raw, truncated) = changed_paths(worktree_path)?;
    let filtered: Vec<String> = raw
        .into_iter()
        .filter(|p| !is_runtime_state_artifact_path(p))
        .collect();
    if filtered.is_empty() && truncated {
        return Err(CommandError::plain(format!(
            "Worktree status for {worktree_path} exceeded scan cap; refusing to treat capped status as clean"
        )));
    }
    Ok((filtered, truncated))
}

/// Executes the automated worktree merge pipeline safely across checked-out and
/// headless target branches:
/// 1. Repository trust, policy & agent-review gate (`harness::guard_command`)
/// 2. Dirty-worktree & parked-operation guards on both source and target checkouts
/// 3. 6-point integration verification + quarantined 3-way `git merge-tree` conflict check
/// 4. Working-tree merge (when target is checked out) or CAS in-memory `merge-tree` + `commit-tree`
/// 5. Process reaping, worktree removal, and source branch cleanup
pub fn merge_worktree_pipeline(
    repo_path: &str,
    source_branch: &str,
    target_branch: Option<&str>,
    squash: bool,
    commit_message: Option<&str>,
    remove_wt: bool,
) -> Result<WorktreeMergeResult, CommandError> {
    let family = resolve_worktree_family(repo_path, repo_path)?;
    let anchor = family.anchor;
    let anchor_str = anchor
        .to_str()
        .ok_or_else(|| CommandError::plain("repository anchor path is not valid UTF-8"))?;
    crate::repository_trust::require(&anchor)?;
    validate_ref_name(source_branch)?;

    let target = match target_branch {
        Some(t) => {
            validate_ref_name(t)?;
            t.to_string()
        }
        None => {
            if let Ok(default) = crate::engine::git_reader::compute_default_base(&anchor).checked()
            {
                if let Some((name, _)) = default.base {
                    name
                } else if git_text(&anchor, &["rev-parse", "--verify", "refs/heads/main"]).is_ok() {
                    "main".to_string()
                } else if git_text(&anchor, &["rev-parse", "--verify", "refs/heads/master"]).is_ok()
                {
                    "master".to_string()
                } else {
                    return Err(CommandError::plain("cannot determine target branch"));
                }
            } else if git_text(&anchor, &["rev-parse", "--verify", "refs/heads/main"]).is_ok() {
                "main".to_string()
            } else if git_text(&anchor, &["rev-parse", "--verify", "refs/heads/master"]).is_ok() {
                "master".to_string()
            } else {
                return Err(CommandError::plain("cannot determine target branch"));
            }
        }
    };

    if source_branch == target {
        return Err(CommandError::plain(format!(
            "cannot merge branch {source_branch:?} into itself"
        )));
    }

    let merge_flag = if squash { "--squash" } else { "--no-edit" };
    crate::harness::guard_command(anchor_str, &["git", "merge", merge_flag, source_branch])
        .map_err(CommandError::from)?;

    let (source_sha, source_wt_path, squashed_sha, final_sha) = {
        let _repo_lock = repo_mutation_lock(&anchor);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let source_ref = format!("refs/heads/{source_branch}");
        let target_ref = format!("refs/heads/{target}");
        let source_sha = git_text(&anchor, &["rev-parse", "--verify", &source_ref])
            .map(|s| s.trim().to_string())?;
        let target_sha = git_text(&anchor, &["rev-parse", "--verify", &target_ref])
            .map(|s| s.trim().to_string())?;

        let worktrees = list_worktrees(anchor_str)?;
        let source_wt = worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(source_branch))
            .cloned();
        if let Some(ref swt) = source_wt {
            let (dirty, _) = user_changed_paths(&swt.path)?;
            if !dirty.is_empty() {
                return Err(CommandError::plain(format!(
                    "Worktree for {source_branch} has {} uncommitted file change(s); commit or stash them before merging",
                    dirty.len()
                )));
            }
        }

        let target_wt = worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(target.as_str()))
            .cloned();
        if let Some(ref twt) = target_wt {
            let (dirty, _) = user_changed_paths(&twt.path)?;
            if !dirty.is_empty() {
                return Err(CommandError::plain(format!(
                    "Target worktree ({target}) has {} uncommitted file change(s); commit or stash them before merging",
                    dirty.len()
                )));
            }
            if let Some(kind) = crate::engine::repo_op::detect_kind(Path::new(&twt.path))? {
                return Err(CommandError::plain(format!(
                    "Target worktree ({target}) has a {} in progress; finish or abort it before merging",
                    kind.label()
                )));
            }
        }

        let check = verify_branch_integration(&anchor, source_branch, &target)?;
        let mut squashed_sha = None;

        if !check.is_integrated {
            let quarantine = crate::engine::object_quarantine::ObjectQuarantine::new(&anchor)
                .map_err(|e| CommandError::plain(e.to_string()))?;
            match quarantine
                .merge_tree(
                    &anchor,
                    &target_sha,
                    &source_sha,
                    Instant::now() + crate::engine::git_cli::DEFAULT_TIMEOUT,
                )
                .map_err(|e| CommandError::plain(e.to_string()))?
            {
                crate::engine::object_quarantine::MergeOutcome::Conflicted {
                    paths,
                    paths_total,
                    ..
                } => {
                    let preview: Vec<&str> = paths.iter().take(5).map(String::as_str).collect();
                    return Err(CommandError::plain(format!(
                        "Merge of {source_branch} into {target} has {paths_total} conflicting file(s): {}",
                        preview.join(", ")
                    )));
                }
                crate::engine::object_quarantine::MergeOutcome::Clean { .. } => {}
            }

            let is_ff = crate::engine::git_cli::git_captured(
                &anchor,
                &["merge-base", "--is-ancestor", &target_sha, &source_sha],
            )
            .map(|r| r.status_code == 0)
            .unwrap_or(false);

            if squash || !is_ff {
                GitWriter::require_commit_identity(&anchor)?;
            }

            let msg = match commit_message {
                Some(m) => m.to_string(),
                None if squash => format!("Squash merge branch '{source_branch}' into {target}"),
                None => format!("Merge branch '{source_branch}' into {target}"),
            };

            let new_commit = if !squash && is_ff {
                source_sha.clone()
            } else {
                let merged_tree = git_text(
                    &anchor,
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--no-messages",
                        &target_sha,
                        &source_sha,
                    ],
                )?
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
                if merged_tree.is_empty() {
                    return Err(CommandError::plain(
                        "git merge-tree did not return a merged tree OID",
                    ));
                }
                let created_commit = if squash {
                    git_text(
                        &anchor,
                        &["commit-tree", &merged_tree, "-p", &target_sha, "-m", &msg],
                    )?
                    .trim()
                    .to_string()
                } else {
                    git_text(
                        &anchor,
                        &[
                            "commit-tree",
                            &merged_tree,
                            "-p",
                            &target_sha,
                            "-p",
                            &source_sha,
                            "-m",
                            &msg,
                        ],
                    )?
                    .trim()
                    .to_string()
                };
                if squash {
                    squashed_sha = Some(created_commit.clone());
                }
                created_commit
            };

            if let Some(ref twt) = target_wt {
                let twt_path = Path::new(&twt.path);
                git_text(twt_path, &["merge", "--ff-only", &new_commit])?;
            } else {
                git_text(
                    &anchor,
                    &["update-ref", &target_ref, &new_commit, &target_sha],
                )?;
            }
        }

        let final_sha = git_text(&anchor, &["rev-parse", "--verify", &target_ref])
            .map(|s| s.trim().to_string())
            .ok();

        let swt_path = source_wt.filter(|w| !w.is_main).map(|w| w.path);
        (source_sha, swt_path, squashed_sha, final_sha)
    };

    let mut wt_removed = false;
    if remove_wt {
        let current_source_sha = git_text(
            &anchor,
            &[
                "rev-parse",
                "--verify",
                &format!("refs/heads/{source_branch}"),
            ],
        )
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

        if current_source_sha == source_sha {
            let mut safe_to_delete_branch = true;
            if let Some(ref wt_path) = source_wt_path {
                let still_clean = user_changed_paths(wt_path)
                    .map(|(dirty, _)| dirty.is_empty())
                    .unwrap_or(false);
                if still_clean {
                    let remove_argv = remove_worktree_argv(wt_path, true);
                    let remove_refs: Vec<&str> = remove_argv.iter().map(String::as_str).collect();
                    if crate::harness::guard_command(anchor_str, &remove_refs).is_ok() {
                        let _ = reap_processes_in_dir(Path::new(wt_path), false);
                        if remove_worktree(anchor_str, wt_path, true).is_ok() {
                            wt_removed = true;
                        } else {
                            safe_to_delete_branch = false;
                        }
                    } else {
                        safe_to_delete_branch = false;
                    }
                } else {
                    safe_to_delete_branch = false;
                }
            }
            if safe_to_delete_branch {
                let del_flag = if squash { "-D" } else { "-d" };
                if crate::harness::guard_command(
                    anchor_str,
                    &["git", "branch", del_flag, source_branch],
                )
                .is_ok()
                {
                    let _ = git_text(&anchor, &["branch", del_flag, source_branch]);
                }
            }
            if wt_removed || safe_to_delete_branch {
                let _ = crate::engine::agent_bus::release_lanes_matching(
                    anchor_str,
                    source_wt_path.as_deref().map(Path::new),
                    Some(source_branch),
                );
            }
        }
    }

    Ok(WorktreeMergeResult {
        ok: true,
        source_branch: source_branch.to_string(),
        target_branch: target,
        squashed_commit: squashed_sha,
        merged_sha: final_sha,
        worktree_removed: wt_removed,
        error: None,
    })
}

const MAX_SPARSE_SCOPE_INPUTS: usize = 64;
const MAX_SPARSE_GRAPH_DEPTH: usize = 3;
const MAX_SPARSE_GRAPH_FILES: usize = 256;

/// Receipt for a DevMap-guided sparse, zero-copy worktree checkout.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SparseWorktreeCreated {
    pub path: String,
    pub branch: String,
    pub head_sha: String,
    pub requested_paths: Vec<String>,
    pub materialized_cone_dirs: Vec<String>,
    pub caches_cloned: Vec<String>,
    pub cache_error: Option<String>,
    pub zero_copy_devmap: bool,
    pub overlay_path: Option<String>,
    /// What seeding the DevMap store did (see [`DevmapOutcome`]); `zero_copy_devmap`
    /// is true only when this is `provisioned` with `zero_copy`.
    pub devmap: DevmapOutcome,
}

fn normalize_sparse_rel_path(raw: &str) -> Result<String, CommandError> {
    let trimmed = raw.trim().replace('\\', "/");
    if trimmed.starts_with('/')
        || Path::new(&trimmed).is_absolute()
        || (trimmed.len() >= 2
            && trimmed.as_bytes()[0].is_ascii_alphabetic()
            && trimmed.as_bytes()[1] == b':')
    {
        return Err(CommandError::plain(format!(
            "sparse scope path {raw:?} must be repository-relative"
        )));
    }
    let stripped = trimmed.trim_end_matches('/');
    if stripped.is_empty() || stripped.contains('\0') || stripped.starts_with('-') {
        return Err(CommandError::plain(format!(
            "invalid sparse scope path {raw:?}"
        )));
    }
    let path = Path::new(stripped);
    let mut parts = Vec::new();
    for comp in path.components() {
        match comp {
            std::path::Component::Normal(os) => {
                let s = os.to_str().ok_or_else(|| {
                    CommandError::plain(format!("sparse scope path {raw:?} is not valid UTF-8"))
                })?;
                if s.is_empty() || s == "." || s == ".." {
                    return Err(CommandError::plain(format!(
                        "sparse scope path {raw:?} contains disallowed segment"
                    )));
                }
                parts.push(s);
            }
            _ => {
                return Err(CommandError::plain(format!(
                    "sparse scope path {raw:?} contains disallowed component"
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

fn cone_dir_for_rel_path(anchor: &Path, rev: Option<&str>, rel: &str) -> Option<String> {
    let tree_rev = rev
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("HEAD");
    let spec = format!("{tree_rev}:{rel}");
    if let Ok(run) = crate::engine::git_cli::git_captured(anchor, &["cat-file", "-t", &spec]) {
        if run.status_code == 0 {
            match String::from_utf8_lossy(&run.stdout).trim() {
                "tree" | "commit" => return Some(rel.to_string()),
                "blob" => {
                    return rel
                        .rsplit_once('/')
                        .and_then(|(parent, _)| (!parent.is_empty()).then(|| parent.to_string()));
                }
                _ => {}
            }
        }
    }

    let candidate = anchor.join(rel);
    if candidate.is_dir() {
        return Some(rel.to_string());
    }
    if candidate.is_file() {
        return rel
            .rsplit_once('/')
            .and_then(|(parent, _)| (!parent.is_empty()).then(|| parent.to_string()));
    }
    match rel.rsplit_once('/') {
        Some((parent, leaf)) if !parent.is_empty() => {
            if leaf.contains('.') && !leaf.starts_with('.') {
                Some(parent.to_string())
            } else {
                Some(rel.to_string())
            }
        }
        _ => {
            // Root-level files are always checked out by `git sparse-checkout --cone`;
            // a single non-existent directory name (including leading-dot dirs like `.github`)
            // is treated as a directory cone.
            if !rel.contains('.') || rel.starts_with('.') {
                Some(rel.to_string())
            } else {
                None
            }
        }
    }
}

fn collapse_redundant_cone_dirs(dirs: std::collections::BTreeSet<String>) -> Vec<String> {
    let mut minimal: Vec<String> = Vec::new();
    for dir in dirs {
        if dir.is_empty() {
            continue;
        }
        let covered = minimal
            .iter()
            .any(|existing| dir == *existing || dir.starts_with(&format!("{existing}/")));
        if !covered {
            minimal.push(dir);
        }
    }
    minimal
}

/// Expands `scope_paths` into a minimal sorted set of Git `--cone` directories by
/// walking outbound file dependencies in `anchor`'s DevMap store (bounded to
/// `MAX_SPARSE_GRAPH_DEPTH` hops and `MAX_SPARSE_GRAPH_FILES` files).
///
/// When `anchor` has no indexed DevMap store yet, falls back deterministically
/// to the normalized cone directories of `scope_paths`.
pub fn expand_sparse_cone_with_devmap(
    anchor: &Path,
    scope_paths: &[String],
) -> Result<Vec<String>, CommandError> {
    expand_sparse_cone_with_devmap_at(anchor, None, scope_paths)
}

fn expand_sparse_cone_with_devmap_at(
    anchor: &Path,
    base: Option<&str>,
    scope_paths: &[String],
) -> Result<Vec<String>, CommandError> {
    if scope_paths.is_empty() {
        return Err(CommandError::plain(
            "sparse scope_paths must contain at least one path",
        ));
    }
    if scope_paths.len() > MAX_SPARSE_SCOPE_INPUTS {
        return Err(CommandError::plain(format!(
            "sparse scope_paths exceeds maximum of {MAX_SPARSE_SCOPE_INPUTS} entries"
        )));
    }

    let mut normalized = Vec::with_capacity(scope_paths.len());
    let mut cone_dirs = std::collections::BTreeSet::new();
    for raw in scope_paths {
        let rel = normalize_sparse_rel_path(raw)?;
        if let Some(cone) = cone_dir_for_rel_path(anchor, base, &rel) {
            cone_dirs.insert(cone);
        }
        normalized.push(rel);
    }

    if let Some(anchor_str) = anchor.to_str() {
        if let Ok(store) = crate::codeintel::open_repo_map(anchor_str) {
            let indexed_files: Vec<String> = store
                .latest_file_hashes()
                .map(|m| m.into_keys().collect())
                .unwrap_or_default();

            let mut visited = std::collections::HashSet::new();
            let mut queue = std::collections::VecDeque::new();

            for req in &normalized {
                let prefix = format!("{req}/");
                for file in &indexed_files {
                    if (file == req || file.starts_with(&prefix))
                        && visited.len() < MAX_SPARSE_GRAPH_FILES
                        && visited.insert(file.clone())
                    {
                        queue.push_back((file.clone(), 0usize));
                    }
                }
            }

            while let Some((curr_file, depth)) = queue.pop_front() {
                if depth >= MAX_SPARSE_GRAPH_DEPTH || visited.len() >= MAX_SPARSE_GRAPH_FILES {
                    continue;
                }
                if let Ok(Some(file_edges)) = store.file_edges(&curr_file, 0.5) {
                    for edge in file_edges.edges {
                        if let Some((parent, _)) = edge.target_file.rsplit_once('/') {
                            if !parent.is_empty() {
                                cone_dirs.insert(parent.to_string());
                            }
                        }
                        if visited.len() < MAX_SPARSE_GRAPH_FILES
                            && visited.insert(edge.target_file.clone())
                        {
                            queue.push_back((edge.target_file, depth + 1));
                        }
                    }
                }
            }
        }
    }

    Ok(collapse_redundant_cone_dirs(cone_dirs))
}

fn clone_scoped_caches_for_cone(
    anchor: &Path,
    worktree: &Path,
    cone_dirs: &[String],
) -> (Vec<String>, Option<String>) {
    let selection = crate::engine::cow_clone::CacheSelection::default();
    let mut cloned_names = Vec::new();
    let mut errors = Vec::new();

    match clone_ignored_caches(anchor, worktree, &selection) {
        Ok(report) => {
            for item in &report.copied {
                if let Some(name) = Path::new(&item.destination)
                    .strip_prefix(worktree)
                    .ok()
                    .and_then(|p| p.to_str())
                {
                    cloned_names.push(name.replace('\\', "/"));
                }
            }
            if let Some(err) = report.error() {
                errors.push(err);
            }
        }
        Err(e) => errors.push(e.into_message()),
    }

    // Also CoW-clone nested `node_modules` inside materialized cone directories
    // (never touching unmaterialized sibling packages).
    for cone in cone_dirs {
        let nested_rel = format!("{cone}/node_modules");
        let src_nm = anchor.join(&nested_rel);
        let dst_nm = worktree.join(&nested_rel);
        if !src_nm.is_dir() || dst_nm.symlink_metadata().is_ok() {
            continue;
        }
        let check_arg = format!("{nested_rel}/");
        let is_ignored = crate::engine::git_cli::git_captured(
            worktree,
            &["check-ignore", "-q", "--", check_arg.as_str()],
        )
        .map(|run| run.status_code == 0)
        .unwrap_or(false);
        if !is_ignored {
            continue;
        }
        if let Some(parent) = dst_nm.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match crate::engine::cow_clone::reflink_copy_dir(&src_nm, &dst_nm) {
            Ok(_) => cloned_names.push(nested_rel),
            Err(e) => errors.push(format!("{nested_rel}: {}", e.message())),
        }
    }

    cloned_names.sort();
    cloned_names.dedup();
    let err_out = (!errors.is_empty()).then(|| errors.join("; "));
    (cloned_names, err_out)
}

/// Creates a zero-copy, DevMap-graph-scoped sparse Git worktree that materializes
/// ONLY root files plus the requested directories and their transitive DevMap
/// code dependencies (`--no-checkout` + per-worktree `sparse-checkout --cone`),
/// paired with a `<512B` zero-copy shared DevMap base pointer (`base_store_ref.json`).
pub fn add_sparse_worktree_zero_copy(
    repo_path: &str,
    target_path: &str,
    branch: &str,
    base: Option<&str>,
    scope_paths: &[String],
    selective_cache: bool,
    gate: GitWriteGate<'_>,
) -> Result<SparseWorktreeCreated, CommandError> {
    add_sparse_worktree_zero_copy_optional_branch(
        repo_path,
        target_path,
        Some(branch),
        base,
        false,
        scope_paths,
        selective_cache,
        true,
        gate,
    )
}

#[allow(clippy::too_many_arguments)]
fn add_sparse_worktree_zero_copy_optional_branch(
    repo_path: &str,
    target_path: &str,
    new_branch: Option<&str>,
    base: Option<&str>,
    detach: bool,
    scope_paths: &[String],
    selective_cache: bool,
    zero_copy_devmap: bool,
    gate: GitWriteGate<'_>,
) -> Result<SparseWorktreeCreated, CommandError> {
    let repo = validate_repo(repo_path)?;
    validate_target_path(target_path)?;
    refuse_gitpulse_lane_target(target_path)?;
    if let Some(branch) = new_branch {
        validate_ref_name(branch)?;
    }
    if let Some(start) = base {
        validate_oid_or_revision(start)?;
    }

    let anchor = resolve_worktree_family(repo_path, repo_path)
        .ok()
        .and_then(|f| f.anchor.to_str().and_then(|s| validate_repo(s).ok()))
        .unwrap_or_else(|| repo.clone());

    let mut normalized_inputs = Vec::with_capacity(scope_paths.len());
    for raw in scope_paths {
        normalized_inputs.push(normalize_sparse_rel_path(raw)?);
    }
    let cone_dirs = expand_sparse_cone_with_devmap_at(&anchor, base, scope_paths)?;
    let target_plain = plain_path_text(target_path);
    let target = Path::new(&target_plain);

    let head_sha = {
        let _repo_lock = repo_mutation_lock(&anchor);
        let _guard = _repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let mut add_args: Vec<&str> = vec!["worktree", "add", "--no-checkout"];
        if let Some(branch) = new_branch {
            add_args.push("-b");
            add_args.push(branch);
        } else if detach {
            add_args.push("--detach");
        }
        add_args.push(&target_plain);
        if let Some(start) = base {
            add_args.push(start);
        }
        gate(&add_args)?;
        git_text(&repo, &add_args)?;

        // `git sparse-checkout init` enables `extensions.worktreeConfig` itself
        // when it must keep the cone per worktree: GitPulse writes no repository
        // config of its own (owner decision ft-7450105d), so there is no
        // `git config` here. That one key, written by git, is the decision's
        // only allowance (owner, 2026-11-03). Every write below is judged
        // before it runs.
        let setup = (|| -> Result<String, CommandError> {
            let init: &[&str] = &["sparse-checkout", "init", "--cone"];
            gate(init)?;
            git_text(target, init)?;
            if !cone_dirs.is_empty() {
                let mut set_args: Vec<&str> = Vec::with_capacity(4 + cone_dirs.len());
                set_args.extend_from_slice(&["sparse-checkout", "set", "--cone", "--"]);
                for dir in &cone_dirs {
                    set_args.push(dir.as_str());
                }
                gate(&set_args)?;
                git_text(target, &set_args)?;
            }
            let checkout: &[&str] = &["checkout", "-q", "HEAD"];
            gate(checkout)?;
            git_text(target, checkout)?;
            git_text(target, &["rev-parse", "--verify", "HEAD"]).map(|s| s.trim().to_string())
        })();

        match setup {
            Ok(sha) => {
                propagate_claude_account_marker(&repo, target);
                sha
            }
            Err(err) => {
                // Roll back, judging each undo like any other write, and say
                // what is still there when an undo is refused or fails.
                let mut left_behind = Vec::new();
                let remove: Vec<&str> = vec!["worktree", "remove", "--force", &target_plain];
                if let Err(e) = gate(&remove).and_then(|()| git_text(&repo, &remove).map(drop)) {
                    left_behind.push(format!(
                        "worktree {target_plain} was not removed: {}",
                        e.message()
                    ));
                } else if let Some(branch) = new_branch {
                    let delete: Vec<&str> = vec!["branch", "-D", branch];
                    if let Err(e) = gate(&delete).and_then(|()| git_text(&repo, &delete).map(drop))
                    {
                        left_behind
                            .push(format!("branch {branch} was not deleted: {}", e.message()));
                    }
                }
                if left_behind.is_empty() {
                    return Err(err);
                }
                return Err(CommandError::plain(format!(
                    "{}; rollback incomplete: {}",
                    err.message(),
                    left_behind.join("; ")
                )));
            }
        }
    };

    let low_space = !zero_copy_devmap
        && matches!(
            crate::engine::cow_clone::read_free(crate::sysload::free_bytes, target),
            crate::sysload::Reading::Value(left) if left < crate::sysload::LOW_DISK_FLOOR
        );

    let ((caches_cloned, cache_error), devmap_outcome, overlay_path) =
        std::thread::scope(|scope| {
            let cache_task = scope.spawn(|| {
                if selective_cache {
                    clone_scoped_caches_for_cone(&anchor, target, &cone_dirs)
                } else {
                    (Vec::new(), None)
                }
            });
            let devmap_task = scope.spawn(|| {
                if zero_copy_devmap {
                    Some(crate::codeintel::provision_worktree_devmap_zero_copy(
                        &anchor, target,
                    ))
                } else if !low_space {
                    Some(crate::codeintel::provision_worktree_devmap(&anchor, target))
                } else {
                    None
                }
            });
            let c = cache_task
                .join()
                .unwrap_or_else(|_| (Vec::new(), Some("selective cache worker panicked".into())));
            let (outcome, overlay) = match devmap_task.join() {
                Ok(Some(res)) => {
                    let overlay = res.as_ref().ok().and_then(|p| p.overlay_path.clone());
                    (DevmapOutcome::from_provision(res), overlay)
                }
                Ok(None) => (
                    DevmapOutcome::Skipped {
                        reason: "skipped local devmap clone under low disk".to_string(),
                    },
                    None,
                ),
                Err(_) => (
                    DevmapOutcome::Failed {
                        error: "devmap provisioning worker panicked".to_string(),
                    },
                    None,
                ),
            };
            (c, outcome, overlay)
        });

    let provisioned_zero_copy = matches!(
        devmap_outcome,
        DevmapOutcome::Provisioned { zero_copy: true }
    );

    Ok(SparseWorktreeCreated {
        path: target_plain,
        branch: new_branch.unwrap_or_default().to_string(),
        head_sha,
        requested_paths: normalized_inputs,
        materialized_cone_dirs: cone_dirs,
        caches_cloned,
        cache_error,
        zero_copy_devmap: provisioned_zero_copy,
        overlay_path,
        devmap: devmap_outcome,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::path::PathBuf;

    #[test]
    fn test_branch_template_rendering_and_sanitization() {
        assert_eq!(
            sanitize_branch_name("feature/add-sync:v1"),
            "feature-add-sync-v1"
        );
        assert_eq!(sanitize_branch_name("---test/foo---"), "test-foo");
        let template = "../{{ repo }}.{{ branch | sanitize }}";
        let rendered =
            render_branch_template(template, "my-repo", "feature/my-work", None, None, None)
                .unwrap();
        assert_eq!(rendered, "../my-repo.feature-my-work");

        let custom = "{{ repo }}/{{ task_id }}/{{ branch | sanitize }}";
        let res = render_branch_template(custom, "my-repo", "feat/123", Some("TASK-1"), None, None)
            .unwrap();
        assert_eq!(res, "my-repo/TASK-1/feat-123");

        assert!(render_branch_template("{{ unknown }}", "r", "b", None, None, None).is_err());
    }

    #[test]
    fn test_six_point_branch_verification_algorithm() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::test_support::git_in(repo, &["init", "-b", "main"]);
        crate::test_support::git_in(repo, &["config", "user.name", "test"]);
        crate::test_support::git_in(repo, &["config", "user.email", "test@test.com"]);
        std::fs::write(repo.join("file1.txt"), "hello").unwrap();
        crate::test_support::git_in(repo, &["add", "."]);
        crate::test_support::git_in(repo, &["commit", "-m", "initial"]);

        // Same commit branch
        crate::test_support::git_in(repo, &["branch", "feat-same"]);
        let res = verify_branch_integration(repo, "feat-same", "main").unwrap();
        assert!(res.same_commit);
        assert!(res.is_ancestor);
        assert!(res.is_integrated);

        // Advance feat-different
        crate::test_support::git_in(repo, &["checkout", "-b", "feat-diff"]);
        std::fs::write(repo.join("file2.txt"), "world").unwrap();
        crate::test_support::git_in(repo, &["add", "."]);
        crate::test_support::git_in(repo, &["commit", "-m", "add file2"]);
        crate::test_support::git_in(repo, &["checkout", "main"]);

        let res_diff = verify_branch_integration(repo, "feat-diff", "main").unwrap();
        assert!(!res_diff.same_commit);
        assert!(!res_diff.is_ancestor);
        assert!(!res_diff.merge_adds_nothing);
        assert!(!res_diff.patch_id_match);
        assert!(!res_diff.is_integrated);

        // Cherry-pick feat-diff onto main PLUS another commit on main so trees differ!
        std::fs::write(repo.join("main_only.txt"), "diverged").unwrap();
        crate::test_support::git_in(repo, &["add", "."]);
        crate::test_support::git_in(repo, &["commit", "-m", "main diverged"]);
        crate::test_support::git_in(repo, &["cherry-pick", "feat-diff"]);

        let res_cherry = verify_branch_integration(repo, "feat-diff", "main").unwrap();
        assert!(!res_cherry.same_commit);
        assert!(!res_cherry.is_ancestor);
        assert!(!res_cherry.trees_match);
        assert!(res_cherry.patch_id_match);
        assert!(res_cherry.merge_adds_nothing);
        assert!(res_cherry.is_integrated);
    }

    #[test]
    fn test_merge_worktree_pipeline_preserves_divergent_target_and_refuses_dirty_or_conflicting_lanes(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::test_support::git_in(repo, &["init", "-b", "main"]);
        crate::test_support::git_in(repo, &["config", "user.name", "Test User"]);
        crate::test_support::git_in(repo, &["config", "user.email", "user@example.com"]);
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        crate::test_support::git_in(repo, &["add", "."]);
        crate::test_support::git_in(repo, &["commit", "-m", "base"]);
        crate::test_support::trust_repo(repo);

        let repo_str = repo.to_str().unwrap();
        let wt_parent = tempfile::tempdir().unwrap();
        let wt_clean = wt_parent.path().join("wt-clean");
        let wt_clean_str = wt_clean.to_str().unwrap();
        add_worktree(
            repo_str,
            wt_clean_str,
            Some("feat/clean"),
            Some("main"),
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(&wt_clean);

        std::fs::write(wt_clean.join("agent_feature.txt"), "agent work\n").unwrap();
        crate::test_support::git_in(&wt_clean, &["add", "."]);
        crate::test_support::git_in(&wt_clean, &["commit", "-m", "agent commit"]);

        // Advance `main` in the primary checkout so `main` diverges from `feat/clean`!
        std::fs::write(repo.join("main_progress.txt"), "concurrent main commit\n").unwrap();
        crate::test_support::git_in(repo, &["add", "."]);
        crate::test_support::git_in(repo, &["commit", "-m", "main commit"]);

        // Uncommitted file in `wt_clean` must block merge_worktree_pipeline.
        std::fs::write(wt_clean.join("uncommitted.txt"), "wip").unwrap();
        let dirty_err =
            merge_worktree_pipeline(repo_str, "feat/clean", Some("main"), true, None, true)
                .expect_err("dirty worktree must be refused");
        assert!(dirty_err.message().contains("uncommitted"));
        std::fs::remove_file(wt_clean.join("uncommitted.txt")).unwrap();

        // Clean squash merge preserves BOTH `main_progress.txt` and `agent_feature.txt`
        // AND keeps `main`'s working tree/index clean!
        let merged = merge_worktree_pipeline(
            repo_str,
            "feat/clean",
            Some("main"),
            true,
            Some("feat: squashed agent work"),
            true,
        )
        .unwrap();
        assert!(merged.ok);
        assert!(merged.worktree_removed);
        assert!(merged.squashed_commit.is_some());
        assert_eq!(
            std::fs::read_to_string(repo.join("main_progress.txt")).unwrap(),
            "concurrent main commit\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("agent_feature.txt")).unwrap(),
            "agent work\n"
        );
        let (main_dirty, _) = user_changed_paths(repo_str).unwrap();
        assert!(
            main_dirty.is_empty(),
            "main working tree must stay clean: {main_dirty:?}"
        );

        // Conflicting branch must fail closed without polluting `main`.
        let wt_conflict = wt_parent.path().join("wt-conflict");
        let wt_conflict_str = wt_conflict.to_str().unwrap();
        add_worktree(
            repo_str,
            wt_conflict_str,
            Some("feat/conflict"),
            Some("main"),
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(&wt_conflict);
        std::fs::write(wt_conflict.join("shared.txt"), "agent side\n").unwrap();
        crate::test_support::git_in(&wt_conflict, &["add", "shared.txt"]);
        crate::test_support::git_in(&wt_conflict, &["commit", "-m", "conflict side A"]);

        std::fs::write(repo.join("shared.txt"), "main side\n").unwrap();
        crate::test_support::git_in(repo, &["add", "shared.txt"]);
        crate::test_support::git_in(repo, &["commit", "-m", "conflict side B"]);

        let conflict_err =
            merge_worktree_pipeline(repo_str, "feat/conflict", Some("main"), true, None, true)
                .expect_err("conflicting merge must fail closed");
        assert!(conflict_err.message().contains("conflicting"));
        let (main_dirty_after, _) = user_changed_paths(repo_str).unwrap();
        assert!(main_dirty_after.is_empty());
        assert!(
            wt_conflict.exists(),
            "conflicting worktree must be preserved"
        );
    }

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
        let scan = scan_worktree(
            &repo,
            &entry,
            ScanDepth::Full,
            &Base::none(),
            None,
            &ScanPass::now(),
        );
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
            scan_worktree(
                &repo,
                &entry,
                ScanDepth::Full,
                &Base::none(),
                None,
                &ScanPass::now(),
            )
        });
        assert_eq!(scan.dirty, Some(0));
        assert_eq!(scan.diff_stat, None);
        let note = scan.note.unwrap_or_default();
        assert!(
            note.starts_with("some measurements were not taken: "),
            "{note}"
        );
        assert!(note.contains("forced by test"), "{note}");
        let whole = scan_worktree(
            &repo,
            &entry,
            ScanDepth::Full,
            &Base::none(),
            None,
            &ScanPass::now(),
        );
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
        // Thread CPU time, not wall time: this took 31 s of wall clock against
        // a scaled 5 s budget at load 30-60 (1.15 s alone) while the thread
        // did the same work. The wall clock measures the machine's load; the
        // thread's CPU time measures the normalisation.
        // The work runs with or without a clock, so a panic on hostile input
        // is still seen; only the CPU bound is skipped.
        let started = crate::sysload::try_thread_cpu();
        for _ in 0..20 {
            for path in &hostile {
                let _ = agent_layout(path);
            }
        }
        crate::sysload::if_clock(
            "agent_layout_stays_linear_on_hostile_input",
            started.map(crate::sysload::cpu_since),
            |elapsed| {
                assert!(
                    elapsed
                        < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(5)),
                    "hostile paths took {elapsed:?} of CPU"
                );
            },
        );
    }

    #[test]
    fn agent_layout_skips_its_cpu_bound_where_there_is_no_thread_cpu_clock() {
        crate::sysload::no_clock::assert_skips(
            "agent_layout_stays_linear_on_hostile_input",
            agent_layout_stays_linear_on_hostile_input,
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
            assert!(
                refused.contains(GITPULSE_LANE_DIR),
                "{refused}",
                refused = refused.message()
            );
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

    fn dirty_repo() -> tempfile::TempDir {
        use crate::test_support::git_in;
        let dir = tempfile::tempdir().expect("repo");
        git_in(dir.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(dir.path().join("tracked.txt"), "t").unwrap();
        git_in(dir.path(), &["add", "."]);
        git_in(dir.path(), &["commit", "-q", "-m", "init"]);
        dir
    }

    /// S3-08. A new directory is listed by its files, whatever the user's
    /// config says: `status.showUntrackedFiles=no` hid them, and the default
    /// collapsed them to `new/`, which no exact-path match can find.
    #[test]
    fn untracked_files_are_listed_individually_whatever_the_users_config_says() {
        let repo = dirty_repo();
        crate::test_support::git_in(repo.path(), &["config", "status.showUntrackedFiles", "no"]);
        std::fs::create_dir_all(repo.path().join("new/dir")).unwrap();
        std::fs::write(repo.path().join("new/dir/x.rs"), "x").unwrap();
        std::fs::write(repo.path().join("new/y.rs"), "y").unwrap();
        let (paths, truncated) = changed_paths_within(repo.path().to_str().unwrap(), None).unwrap();
        assert!(!truncated);
        let mut paths = paths;
        paths.sort();
        assert_eq!(paths, ["new/dir/x.rs", "new/y.rs"]);
    }

    /// S3-08. A tree of untracked files far past the path cap stays within it,
    /// and says it was cut.
    #[test]
    fn a_huge_untracked_tree_stays_within_the_path_cap_and_says_it_was_cut() {
        let repo = dirty_repo();
        let dir = repo.path().join("gen");
        std::fs::create_dir_all(&dir).unwrap();
        for n in 0..(MAX_CHANGED_PATHS * 8) {
            std::fs::write(dir.join(format!("f{n:05}.txt")), "").unwrap();
        }
        let (paths, truncated) = changed_paths_within(repo.path().to_str().unwrap(), None).unwrap();
        assert_eq!(paths.len(), MAX_CHANGED_PATHS);
        assert!(truncated);
    }

    /// The path-limited read is limited to exactly that path: glob characters
    /// in a name never widen it, and another dirty file is not reported.
    #[test]
    fn a_path_limited_status_names_only_that_literal_path() {
        let repo = dirty_repo();
        for name in ["a[1].txt", "a1.txt", "other.txt"] {
            std::fs::write(repo.path().join(name), "x").unwrap();
        }
        std::fs::write(repo.path().join("tracked.txt"), "changed").unwrap();
        let dir = repo.path().to_str().unwrap();
        for (ask, expect) in [
            ("a[1].txt", vec!["a[1].txt"]),
            ("a1.txt", vec!["a1.txt"]),
            ("tracked.txt", vec!["tracked.txt"]),
            ("clean-and-absent.txt", vec![]),
        ] {
            let (paths, truncated) = changed_paths_only(dir, ask, None).unwrap();
            assert_eq!(
                (paths, truncated),
                (expect.iter().map(|s| s.to_string()).collect(), false),
                "{ask}"
            );
        }
        // A deadline already passed reads nothing and says why, never "clean".
        let expired = ScanDeadline::after(Instant::now(), Duration::ZERO);
        let error = changed_paths_only(dir, "tracked.txt", Some(&expired)).unwrap_err();
        assert_eq!(error, DEADLINE_PASSED);
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

    /// Both callers share `merge_tree::integrate`. The checkout merge argv is
    /// built in one place, and the object-database owner does not build one.
    #[test]
    fn lane_integration_has_one_merge_argv_builder() {
        let needle = format!("--{}-{}", "ff", "only");
        let owner = include_str!("merge_tree.rs");
        assert!(
            !owner.contains(&needle),
            "the object-database owner builds a checkout merge"
        );
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the crate lives in src-tauri");
        for rel in [
            "src/lib/workbench/waveLaunch.ts",
            "src/lib/workbench/attemptWorktree.ts",
            "src/lib/components/WorktreesPanel.svelte",
            "src-tauri/src/commands/worktrees.rs",
        ] {
            let text = std::fs::read_to_string(root.join(rel))
                .unwrap_or_else(|error| panic!("{rel}: {error}"));
            assert!(!text.contains(&needle), "{rel} builds a checkout merge");
        }
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

    fn allow_all(_: &[&str]) -> Result<(), CommandError> {
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

    /// The target ref moves even when the main checkout is on another branch.
    /// That checkout's files stay, and the result says so. It used to refuse
    /// unless the checkout was already on the target, because the merge ran there.
    #[test]
    fn teardown_moves_the_target_when_the_main_checkout_is_not_on_it() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        git_text(repo, &["switch", "-c", "elsewhere"]).unwrap();
        let head = rev(repo, "HEAD");
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
        .expect("the target ref moves without the checkout");
        assert!(
            asked
                .iter()
                .any(|a| a.starts_with("update-ref refs/heads/main ")),
            "the compare-and-swap was not asked about: {asked:?}"
        );
        assert!(!result.checkout_advanced);
        let note = result.checkout_note.expect("the checkout was left");
        assert!(note.contains("checkout not advanced"), "{note}");
        assert!(
            !repo.join("feature.txt").exists(),
            "the other branch gained the file"
        );
        assert_eq!(rev(repo, "HEAD"), head);
        assert_eq!(
            git_text(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
                .unwrap()
                .trim(),
            "elsewhere"
        );
        let landed = git_text(repo, &["ls-tree", "-r", "--name-only", "refs/heads/main"]).unwrap();
        assert!(landed.lines().any(|line| line == "feature.txt"), "{landed}");
        assert!(!wt.path().exists() && !branch_exists(repo, "feature-x"));
    }

    /// Older git (no `merge-tree --write-tree`): the squash is committed in the
    /// checkout, where a pre-commit hook can refuse it. The commit's failure
    /// stops before anything is torn down and the squash stays staged to finish
    /// or undo.
    #[cfg(unix)]
    #[test]
    fn an_older_git_squash_whose_commit_fails_keeps_the_worktree_and_the_branch() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let hook = repo.join(".git/hooks/pre-commit");
        std::fs::write(&hook, "#!/bin/sh\necho refusing this commit >&2\nexit 1\n").unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        let refused = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            true,
            None,
            object_config(false, false),
            None,
            &mut allow_all,
            &mut |_| {},
            &mut delete_dir_tree,
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
        // `git hook list` runs only on a git that has it (2.54+), once per commit hook.
        asked.retain(|a| !a.starts_with("hook list "));
        assert_eq!(asked.len(), 7, "{asked:?}");
        assert!(
            asked[0].starts_with("merge-tree --write-tree --name-only --no-messages -z "),
            "{asked:?}"
        );
        assert!(asked[1].starts_with("commit-tree "), "{asked:?}");
        assert_eq!(asked[1].matches(" -p ").count(), 1, "{asked:?}");
        assert!(
            asked[1].contains("Merge branch 'feature-x' (squashed)"),
            "{asked:?}"
        );
        assert!(
            asked[2].starts_with("update-ref refs/heads/main "),
            "{asked:?}"
        );
        assert_eq!(asked[3], "update-index -q --refresh");
        assert!(asked[4].starts_with("read-tree -m -u "), "{asked:?}");
        assert_eq!(asked[5], format!("worktree remove --force {wt_path}"));
        assert_eq!(asked[6], "branch -D feature-x");
        assert_eq!(commit_parents(repo, "HEAD").len(), 1);

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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
            candidate,
        } = refused
        else {
            panic!("a conflict was reported as {refused:?}");
        };
        assert_eq!(paths, ["README.md"], "{message}");
        assert_eq!((total, rolled_back, rollback_error), (1, true, None));
        let candidate = candidate.expect("the conflict names the merge tree");
        assert_eq!(
            git_text(repo, &["cat-file", "-t", &candidate])
                .unwrap()
                .trim(),
            "tree",
            "{candidate}"
        );
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

    /// Parents of `name`, in order, from `rev-list --parents`.
    fn commit_parents(repo: &Path, name: &str) -> Vec<String> {
        git_text(repo, &["rev-list", "--parents", "-n", "1", name])
            .unwrap()
            .split_whitespace()
            .skip(1)
            .map(str::to_string)
            .collect()
    }

    /// `main` gains a file the lane does not have, so the histories diverge
    /// and the integration records a merge commit, without conflicting.
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
        assert_eq!(result.candidate_oid.as_deref(), Some(tip.as_str()));
        assert!(result.checkout_advanced && !result.checkout_fallback);
        assert_eq!(result.checkout_note, None);
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

    #[test]
    fn a_parked_squash_refuses_the_next_merge_and_keeps_the_lane() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        // Plant the parked squash directly. The object-database path does not
        // leave one behind when its own commit is refused, and a checkout
        // pre-commit hook does not run for commit-tree.
        let planted = git_captured_status(repo, &["merge", "--squash", "feature-x"]);
        assert_eq!(planted, 0, "the fixture squash did not stage");
        assert_eq!(
            crate::engine::repo_op::detect_kind(repo).unwrap(),
            Some(crate::engine::repo_op::OperationKind::SquashMerge)
        );
        let before = rev(repo, "HEAD");
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
        assert_eq!(rev(repo, "HEAD"), before);
        assert!(wt.path().join("feature.txt").exists() && branch_exists(repo, "feature-x"));
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

    /// Adversarial L4: `batch_divergence` is fresh after each ref move, and a git
    /// that rejects the atom answers None with exactly one spawn.
    #[test]
    fn l4_batch_divergence_is_fresh_after_moves_and_none_without_a_retry() {
        let _held = crate::engine::ref_cache::retention();
        let (main, _parent, linked) = repo_with_worktrees(1);
        let root = plain(main.path());
        let truth = |base: &str, b: &str| {
            let c = rev_text(
                &root,
                &[
                    "rev-list",
                    "--left-right",
                    "--count",
                    &format!("{base}...{b}"),
                ],
            );
            let mut it = c.split_whitespace().map(|n| n.parse::<usize>().unwrap());
            let behind = it.next().unwrap();
            let ahead = it.next().unwrap();
            WorktreeDivergence { ahead, behind }
        };
        crate::engine::ref_cache::eager(|| {
            let ask = || {
                let base = rev(&root, "main");
                (
                    base.clone(),
                    batch_divergence(&root, &base, &["agent/00", "main"]).expect("a batch"),
                )
            };
            let check = |label: &str| {
                let (base, got) = ask();
                for b in ["agent/00", "main"] {
                    assert_eq!(got[b], truth(&base, b), "stale {b} after {label}");
                }
            };
            check("warm");
            check("warm again");
            std::fs::write(linked[0].join("x.txt"), "x").unwrap();
            git_in(&linked[0], &["add", "."]);
            git_in(&linked[0], &["commit", "-q", "-m", "x"]);
            check("a commit in the linked worktree");
            git_in(
                &root,
                &["commit", "-q", "--allow-empty", "-m", "main moves"],
            );
            check("the base moving");
            let tip = rev(&root, "agent/00");
            git_in(
                &root,
                &["update-ref", "refs/heads/agent/00", &rev(&root, "main")],
            );
            check("update-ref back to base");
            git_in(&root, &["pack-refs", "--all"]);
            check("pack-refs");
            git_in(&root, &["update-ref", "refs/heads/agent/00", &tip]);
            check("loose over packed");
        });
        let base = rev(&root, "main");
        let arg = format!("--format=%(refname)%00%(ahead-behind:{base})");
        let retries = || {
            crate::engine::git_cli::spawn_log::spawns_in(&root)
                .iter()
                .filter(|argv| argv.iter().any(|a| a == "--format=%(refname)"))
                .count()
        };
        let before = retries();
        let got = crate::engine::git_cli::with_forced_spawn_failure_of(&arg, || {
            batch_divergence(&root, &base, &["main"])
        });
        assert_eq!(got, None);
        assert_eq!(retries(), before, "no atom-less retry");
    }

    fn rev_text(repo: &Path, args: &[&str]) -> String {
        git_text(repo, args).unwrap().trim().to_string()
    }

    /// Pins `batch_divergence` on worktrees whose branches are diverged, one of
    /// them named with unicode, slashes and `@`: exact-name rows only, a
    /// namespace prefix and a missing branch absent, and `None` for a base git
    /// cannot measure.
    #[test]
    fn batch_divergence_pins_exact_branch_rows() {
        let (main, _parent, linked) = repo_with_worktrees(2);
        let root = plain(main.path());
        let odd = "w\u{fc}/\u{e7}@x";
        git_in(&root, &["branch", odd]);
        let odd_tree = _parent.path().join("odd-tree");
        git_in(
            &root,
            &["worktree", "add", "-q", odd_tree.to_str().unwrap(), odd],
        );
        for (tree, n) in [(&linked[0], 2), (&linked[1], 1), (&odd_tree, 3)] {
            for i in 0..n {
                std::fs::write(tree.join(format!("c{i}.txt")), "x").unwrap();
                git_in(tree, &["add", "."]);
                git_in(tree, &["commit", "-q", "-m", &format!("c{i}")]);
            }
        }
        std::fs::write(root.join("m.txt"), "x").unwrap();
        git_in(&root, &["add", "."]);
        git_in(&root, &["commit", "-q", "-m", "main moves"]);
        let base = rev(&root, "main");

        let asked = ["agent/00", "agent/01", odd, "main", "agent", "gone/branch"];
        let got = batch_divergence(&root, &base, &asked).expect("a batch");
        let pair = |a, b| WorktreeDivergence {
            ahead: a,
            behind: b,
        };
        let expected = HashMap::from([
            ("agent/00".to_string(), pair(2, 1)),
            ("agent/01".to_string(), pair(1, 1)),
            (odd.to_string(), pair(3, 1)),
            ("main".to_string(), pair(0, 0)),
        ]);
        assert_eq!(
            got, expected,
            "`agent` is a prefix and `gone/branch` absent"
        );
        assert_eq!(batch_divergence(&root, &base, &[]), Some(HashMap::new()));
        let missing = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(batch_divergence(&root, missing, &["main"]), None);
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
        let branch_tip = rev(wt.path(), "HEAD");
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
        // `git hook list` runs only on a git that has it (2.54+), once per commit hook.
        asked.retain(|a| !a.starts_with("hook list "));
        assert_eq!(asked.len(), 7, "{asked:?}");
        assert!(
            asked[0].starts_with("merge-tree --write-tree --name-only --no-messages -z "),
            "{asked:?}"
        );
        assert!(asked[1].starts_with("commit-tree "), "{asked:?}");
        assert_eq!(asked[1].matches(" -p ").count(), 2, "{asked:?}");
        assert!(
            asked[1].contains("Merge branch 'feature-x' into main"),
            "{asked:?}"
        );
        assert!(
            asked[2].starts_with("update-ref refs/heads/main "),
            "{asked:?}"
        );
        assert_eq!(asked[3], "update-index -q --refresh");
        assert!(asked[4].starts_with("read-tree -m -u "), "{asked:?}");
        assert_eq!(asked[5], format!("worktree remove --force {wt_path}"));
        assert_eq!(asked[6], "branch -d feature-x");
        let parents = commit_parents(repo, "HEAD");
        assert_eq!(parents, vec![result.before_oid.clone(), branch_tip]);
        assert!(result.worktree_removed && result.branch_deleted);
        assert_ne!(
            result.after_oid.as_deref(),
            Some(result.before_oid.as_str())
        );
        assert!(repo.join("feature.txt").exists() && repo.join("main-only.txt").exists());
    }

    fn object_config(
        require_write_tree: bool,
        supported: bool,
    ) -> crate::engine::merge_tree::IntegrateConfig {
        crate::engine::merge_tree::IntegrateConfig {
            require_write_tree,
            write_tree: if supported {
                crate::engine::merge_tree::WriteTreeMode::ForceSupported
            } else {
                crate::engine::merge_tree::WriteTreeMode::ForceUnsupported
            },
        }
    }

    /// Main renames `foo` to `bar` while the lane adds a call to `foo`. The
    /// textual merge is clean and would not compile. The candidate is rejected
    /// and main is unchanged.
    #[test]
    fn a_red_candidate_is_never_merged() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        std::fs::write(repo.join("x.ts"), "function foo() { return 1; }\n").unwrap();
        std::fs::write(repo.join("y.ts"), "// callers\n").unwrap();
        crate::test_support::git_in(repo, &["add", "x.ts", "y.ts"]);
        crate::test_support::git_in(repo, &["commit", "-qm", "symbols"]);
        crate::test_support::git_in(wt.path(), &["merge", "-q", "--no-edit", "main"]);
        std::fs::write(repo.join("x.ts"), "function bar() { return 1; }\n").unwrap();
        crate::test_support::git_in(repo, &["commit", "-qam", "rename foo to bar"]);
        std::fs::write(wt.path().join("y.ts"), "foo();\n").unwrap();
        crate::test_support::git_in(wt.path(), &["commit", "-qam", "call foo"]);
        let before = rev(repo, "HEAD");
        let porcelain = git_text(repo, &["status", "--porcelain"]).unwrap();
        let mut verify = |path: &Path, _oid: &str| {
            let source = std::fs::read_to_string(path.join("x.ts")).unwrap_or_default();
            let caller = std::fs::read_to_string(path.join("y.ts")).unwrap_or_default();
            if caller.contains("foo(") && !source.contains("function foo") {
                Err("y.ts calls foo but x.ts no longer defines it".to_string())
            } else {
                Ok(())
            }
        };
        let refused = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            object_config(true, true),
            Some(&mut verify),
            &mut allow_all,
            &mut |_| {},
            &mut delete_dir_tree,
        )
        .unwrap_err();
        let MergeTeardownError::CandidateFailed { message, candidate } = refused else {
            panic!("{refused:?}");
        };
        assert!(message.contains("no longer defines"), "{message}");
        assert_eq!(
            git_text(repo, &["cat-file", "-t", &candidate])
                .unwrap()
                .trim(),
            "commit"
        );
        assert_ne!(candidate, before);
        assert_eq!(rev(repo, "HEAD"), before);
        assert_eq!(
            git_text(repo, &["status", "--porcelain"]).unwrap(),
            porcelain
        );
        assert!(std::fs::read_to_string(repo.join("x.ts"))
            .unwrap()
            .contains("function bar"));
        assert!(!std::fs::read_to_string(repo.join("y.ts"))
            .unwrap()
            .contains("foo("));
        main_checkout_is_idle(repo);
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
        assert_eq!(
            list_worktrees(repo.to_str().unwrap()).unwrap().len(),
            2,
            "the scratch worktree was left registered"
        );
    }

    /// A verifier that accepts a combined tree lets the ref move, and the
    /// scratch checkout does not stay registered.
    #[test]
    fn an_accepted_candidate_is_merged_and_its_scratch_worktree_is_removed() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        diverge_main(&dir);
        let mut verify = |_path: &Path, _oid: &str| Ok(());
        let result = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            object_config(true, true),
            Some(&mut verify),
            &mut allow_all,
            &mut |_| {},
            &mut delete_dir_tree,
        )
        .unwrap();
        assert!(repo.join("feature.txt").exists());
        assert!(repo.join("main-only.txt").exists());
        assert!(result.worktree_removed);
        assert_eq!(list_worktrees(repo.to_str().unwrap()).unwrap().len(), 1);
        let listed = git_text(repo, &["worktree", "list", "--porcelain"]).unwrap();
        assert!(!listed.contains("gitpulse-candidate"), "{listed}");
    }

    /// One repository, three lanes: a clean integration, a textual conflict,
    /// and a candidate the verifier rejects. Each reports its own outcome and
    /// oid, and only the clean one moves main.
    #[test]
    fn three_lanes_report_merged_conflict_and_candidate_failed() {
        let (dir, clean) = teardown_fixture(None);
        let repo = dir.path();
        let tip = rev(clean.path(), "HEAD");
        let merged = teardown(&dir, &clean, false, None).unwrap();
        assert_eq!(merged.candidate_oid.as_deref(), Some(tip.as_str()));
        assert_eq!(
            git_text(repo, &["cat-file", "-t", &tip]).unwrap().trim(),
            "commit"
        );

        let conflict = tempfile::tempdir().unwrap();
        add_worktree_extended(
            repo.to_str().unwrap(),
            conflict.path().to_str().unwrap(),
            Some("feature-c"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(conflict.path());
        std::fs::write(conflict.path().join("README.md"), "lane edit\n").unwrap();
        crate::test_support::git_in(conflict.path(), &["commit", "-qam", "lane edits README"]);
        std::fs::write(repo.join("README.md"), "main edit\n").unwrap();
        crate::test_support::git_in(repo, &["commit", "-qam", "main edits README"]);
        let before = rev(repo, "HEAD");
        let porcelain = git_text(repo, &["status", "--porcelain"]).unwrap();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            conflict.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut allow_all,
        )
        .unwrap_err();
        let MergeTeardownError::Conflict {
            paths, candidate, ..
        } = refused
        else {
            panic!("{refused:?}");
        };
        assert_eq!(paths, ["README.md"]);
        let tree = candidate.expect("a conflict names its tree");
        assert_eq!(
            git_text(repo, &["cat-file", "-t", &tree]).unwrap().trim(),
            "tree"
        );
        assert_eq!(rev(repo, "HEAD"), before);
        assert_eq!(
            git_text(repo, &["status", "--porcelain"]).unwrap(),
            porcelain
        );
        main_checkout_is_idle(repo);
        assert!(conflict.path().exists() && branch_exists(repo, "feature-c"));

        let red = tempfile::tempdir().unwrap();
        add_worktree_extended(
            repo.to_str().unwrap(),
            red.path().to_str().unwrap(),
            Some("feature-r"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(red.path());
        std::fs::write(red.path().join("red.txt"), "red\n").unwrap();
        crate::test_support::git_in(red.path(), &["add", "red.txt"]);
        crate::test_support::git_in(red.path(), &["commit", "-qm", "red"]);
        let before = rev(repo, "HEAD");
        let mut verify = |_path: &Path, oid: &str| Err(format!("red {oid}"));
        let refused = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            red.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            object_config(true, true),
            Some(&mut verify),
            &mut allow_all,
            &mut |_| {},
            &mut delete_dir_tree,
        )
        .unwrap_err();
        let MergeTeardownError::CandidateFailed { candidate, message } = refused else {
            panic!("{refused:?}");
        };
        assert!(message.contains(&candidate), "{message}");
        assert_eq!(
            git_text(repo, &["cat-file", "-t", &candidate])
                .unwrap()
                .trim(),
            "commit"
        );
        assert_eq!(rev(repo, "HEAD"), before);
        assert!(!repo.join("red.txt").exists());
        main_checkout_is_idle(repo);
        assert!(red.path().exists() && branch_exists(repo, "feature-r"));
    }

    /// The compare-and-swap loses when the target moves after the candidate
    /// was built. The attacker's tip stays; the candidate is not the tip.
    #[test]
    fn a_target_that_moves_during_the_compare_and_swap_is_refused() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path().to_path_buf();
        diverge_main(&dir);
        let before = rev(&repo, "HEAD");
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                if argv.first() == Some(&"update-ref") {
                    crate::test_support::git_in(
                        &repo,
                        &["commit", "-q", "--allow-empty", "-m", "raced"],
                    );
                }
                Ok(())
            },
        )
        .unwrap_err();
        let MergeTeardownError::Refused { message } = refused else {
            panic!("{refused:?}");
        };
        assert!(
            message.contains("nothing was written to refs/heads/main"),
            "{message}"
        );
        assert_ne!(rev(&repo, "refs/heads/main"), before);
        let landed = git_text(&repo, &["ls-tree", "-r", "--name-only", "refs/heads/main"]).unwrap();
        assert!(
            !landed.lines().any(|line| line == "feature.txt"),
            "{landed}"
        );
        assert!(!repo.join("feature.txt").exists());
        main_checkout_is_idle(&repo);
        assert!(wt.path().exists() && branch_exists(&repo, "feature-x"));
    }

    /// Data safety: a pre_merge hook that fails stops the integration before
    /// any object is written or any ref is moved, and the lane is kept.
    #[cfg(unix)]
    #[test]
    fn a_failed_pre_merge_hook_writes_no_commit_and_moves_no_ref() {
        let (dir, wt) = teardown_fixture(Some(
            r#"{"worktree":{"pre_merge":["echo not today >&2; exit 3"]}}"#,
        ));
        let repo = dir.path();
        diverge_main(&dir);
        let main_before = rev(repo, "refs/heads/main");
        let lane_before = rev(repo, "refs/heads/feature-x");
        let mut gated: Vec<String> = Vec::new();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                gated.push(argv.join(" "));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(format!("{refused:?}").contains("pre_merge"), "{refused:?}");
        assert_eq!(rev(repo, "refs/heads/main"), main_before);
        assert_eq!(rev(repo, "refs/heads/feature-x"), lane_before);
        assert!(
            !gated.iter().any(|a| a.starts_with("merge-tree")
                || a.starts_with("commit-tree")
                || a.starts_with("update-ref")),
            "{gated:?}"
        );
        // The hook run records itself in an untracked ledger; tracked files and
        // the merge control files are what an integration would have touched.
        let tracked = git_text(repo, &["status", "--porcelain", "--untracked-files=no"]).unwrap();
        assert!(tracked.trim().is_empty(), "{tracked}");
        let git_dir = crate::engine::git_cli::resolve_git_dir(repo).unwrap();
        for control in ["MERGE_HEAD", "SQUASH_MSG", "MERGE_MSG"] {
            assert!(!git_dir.join(control).exists(), "{control} was left behind");
        }
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
    }

    /// Data safety: a conflicting merge-tree never reaches commit-tree or
    /// update-ref, so no ref moves and both tips stay reachable.
    #[test]
    fn a_conflicting_merge_tree_never_reaches_commit_tree_or_update_ref() {
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let main_before = rev(repo, "refs/heads/main");
        let lane_before = rev(repo, "refs/heads/feature-x");
        let mut gated: Vec<String> = Vec::new();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                gated.push(argv.join(" "));
                Ok(())
            },
        )
        .unwrap_err();
        assert!(
            matches!(refused, MergeTeardownError::Conflict { .. }),
            "{refused:?}"
        );
        assert!(
            gated.iter().any(|a| a.starts_with("merge-tree")),
            "{gated:?}"
        );
        assert!(
            !gated
                .iter()
                .any(|a| a.starts_with("commit-tree") || a.starts_with("update-ref")),
            "{gated:?}"
        );
        assert_eq!(rev(repo, "refs/heads/main"), main_before);
        assert_eq!(rev(repo, "refs/heads/feature-x"), lane_before);
        main_checkout_is_idle(repo);
    }

    /// The wave will not fall back to merging in the checkout.
    #[test]
    fn the_wave_refuses_when_merge_tree_cannot_write() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let before = rev(repo, "HEAD");
        let porcelain = git_text(repo, &["status", "--porcelain"]).unwrap();
        let mut asked = Vec::new();
        let refused = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            object_config(true, false),
            None,
            &mut |argv: &[&str]| {
                asked.push(argv.join(" "));
                Ok(())
            },
            &mut |_| {},
            &mut delete_dir_tree,
        )
        .unwrap_err();
        let message = refused.to_string();
        assert!(message.contains("merge-tree --write-tree"), "{message}");
        assert!(message.contains("Nothing was merged"), "{message}");
        assert!(asked.is_empty(), "{asked:?}");
        assert_eq!(rev(repo, "HEAD"), before);
        assert_eq!(
            git_text(repo, &["status", "--porcelain"]).unwrap(),
            porcelain
        );
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
    }

    /// The panel, on the same git, takes the in-checkout path and says so.
    #[test]
    fn an_older_git_merges_in_the_checkout_and_says_so() {
        let (dir, wt) = teardown_fixture(None);
        let repo = dir.path();
        let mut asked = Vec::new();
        let result = merge_and_teardown_configured(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            object_config(false, false),
            None,
            &mut |argv: &[&str]| {
                asked.push(argv.join(" "));
                Ok(())
            },
            &mut |_| {},
            &mut delete_dir_tree,
        )
        .unwrap();
        assert!(result.checkout_fallback && result.checkout_advanced);
        assert_eq!(result.candidate_oid, None);
        let note = result.checkout_note.expect("the fallback is visible");
        assert!(note.contains("merged in the checkout"), "{note}");
        assert!(note.contains("merge-tree --write-tree"), "{note}");
        assert_eq!(asked[0], merge_teardown_argv("feature-x", false).join(" "));
        assert!(repo.join("feature.txt").exists());
        assert!(!wt.path().exists());
    }

    /// A lane whose base the default branch has moved past still lands, as a
    /// merge commit or a squash, instead of stopping because it is not a
    /// fast-forward.
    #[test]
    fn a_lane_integrates_after_the_default_branch_moves() {
        for squash in [false, true] {
            let (dir, wt) = teardown_fixture(None);
            let repo = dir.path();
            let base = rev(repo, "main");
            write_base_record(wt.path(), "main", &base).unwrap();
            let branch_tip = rev(wt.path(), "HEAD");
            diverge_main(&dir);
            let result = teardown(&dir, &wt, squash, None).expect("integrated");
            assert!(result.merged && result.checkout_advanced && !result.checkout_fallback);
            assert!(repo.join("feature.txt").exists() && repo.join("main-only.txt").exists());
            let parents = commit_parents(repo, "HEAD");
            if squash {
                assert_eq!(parents, vec![result.before_oid.clone()], "squash");
            } else {
                assert_eq!(
                    parents,
                    vec![result.before_oid.clone(), branch_tip],
                    "merge"
                );
            }
        }
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
        assert!(
            refused.contains("locked"),
            "{refused}",
            refused = refused.message()
        );
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
            candidate_oid: Some("c".repeat(40)),
            checkout_advanced: true,
            checkout_fallback: false,
            checkout_note: Some("checkout not advanced".into()),
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
                "candidate_oid",
                "checkout_advanced",
                "checkout_fallback",
                "checkout_note",
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
            serde_json::from_value::<MergeTeardownResult>(value.clone()).unwrap(),
            result
        );
        // A result written before these fields existed still decodes.
        let mut slim = value;
        for key in [
            "candidate_oid",
            "checkout_advanced",
            "checkout_fallback",
            "checkout_note",
        ] {
            slim.as_object_mut().unwrap().remove(key);
        }
        let decoded = serde_json::from_value::<MergeTeardownResult>(slim).unwrap();
        assert_eq!(decoded.candidate_oid, None);
        assert!(!decoded.checkout_advanced && !decoded.checkout_fallback);
        assert_eq!(decoded.checkout_note, None);

        let cases: Vec<(MergeTeardownError, &str, Vec<&str>)> = vec![
            (
                MergeTeardownError::Conflict {
                    message: "m".into(),
                    paths: vec!["a".into()],
                    total: 1,
                    rolled_back: true,
                    rollback_error: None,
                    candidate: None,
                },
                "conflict",
                vec![
                    "candidate",
                    "code",
                    "message",
                    "paths",
                    "rollback_error",
                    "rolled_back",
                    "total",
                ],
            ),
            (
                MergeTeardownError::CandidateFailed {
                    message: "m".into(),
                    candidate: "c".repeat(40),
                },
                "candidate_failed",
                vec!["candidate", "code", "message"],
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
            spawned_with(std::slice::from_ref(&root), "refs/remotes/origin/HEAD") == 1,
            "expected exactly one default-base lookup"
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

    /// Portless's `routes.json` is the machine's, not a worktree's: a full
    /// listing reads it once however many worktrees it scans, a lighter one
    /// not at all, and each route still lands on the worktree it names.
    #[test]
    fn a_listing_reads_the_portless_routes_once_whatever_the_worktree_count() {
        let (main, _parent, _linked) = repo_with_worktrees(3);
        let root = plain(main.path());
        let state = tempfile::tempdir().unwrap();
        std::fs::write(
            state.path().join("routes.json"),
            r#"[{"hostname": "agent-01.app.localhost", "port": 4301},
                {"hostname": "agent-0.app.localhost", "port": 4302}]"#,
        )
        .unwrap();
        let serial = crate::test_support::env::env_serial();
        let _env =
            crate::test_support::env::bind_env(&serial).set("PORTLESS_STATE_DIR", state.path());

        let lighter = list_worktrees_scanned(root.to_str().unwrap(), ScanDepth::Dirty).unwrap();
        assert!(lighter.iter().all(|w| w.active_routes.is_empty()));
        assert_eq!(
            crate::engine::portless::read_log::routes_reads(state.path()),
            0
        );

        let full = list_worktrees(root.to_str().unwrap()).expect("list");
        assert_eq!(full.len(), 4);
        assert_eq!(
            crate::engine::portless::read_log::routes_reads(state.path()),
            1,
            "one read for {} worktrees",
            full.len()
        );
        let claimed: Vec<(Option<&str>, Vec<&str>)> = full
            .iter()
            .map(|w| {
                (
                    w.branch.as_deref(),
                    w.active_routes
                        .iter()
                        .filter(|r| r.is_portless)
                        .map(|r| r.hostname.as_str())
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, routes)| !routes.is_empty())
            .collect();
        assert_eq!(
            claimed,
            vec![(Some("agent/01"), vec!["agent-01.app.localhost"])]
        );
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
    ) -> std::thread::JoinHandle<Result<Vec<WorktreeInfo>, CommandError>> {
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

    /// Staged or untracked work in a checkout that has the target is the
    /// person's, and moving the target would change it underneath them. The
    /// integration is refused with a typed reason, the checkout keeps its
    /// branch (nothing is detached), and the target, the lane and the porcelain
    /// are exactly as they were.
    #[test]
    fn adv_a_dirty_target_checkout_refuses_whatever_the_dirt_is() {
        for squash in [false, true] {
            for dirt in ["staged", "untracked-over-lane-file"] {
                let (dir, wt) = teardown_fixture(None);
                let repo = dir.path();
                diverge_main(&dir);
                if dirt == "staged" {
                    std::fs::write(repo.join("staged.txt"), "user staged\n").unwrap();
                    crate::test_support::git_in(repo, &["add", "staged.txt"]);
                } else {
                    std::fs::write(repo.join("feature.txt"), "user's own feature.txt\n").unwrap();
                }
                let before = rev(repo, "HEAD");
                let porcelain = git_text(repo, &["status", "--porcelain"]).unwrap();
                let refused = teardown(&dir, &wt, squash, None).unwrap_err();
                let MergeTeardownError::Held { reason, path, .. } = refused else {
                    panic!("squash={squash} {dirt}: {refused:?}");
                };
                assert_eq!(
                    reason,
                    crate::engine::merge_tree::HeldReason::CheckoutDirty,
                    "squash={squash} {dirt}"
                );
                assert!(path.is_some());
                assert_eq!(
                    rev(repo, "refs/heads/main"),
                    before,
                    "squash={squash} {dirt}"
                );
                assert_eq!(
                    git_text(repo, &["symbolic-ref", "-q", "HEAD"])
                        .unwrap()
                        .trim(),
                    "refs/heads/main",
                    "squash={squash} {dirt}: the checkout was detached"
                );
                assert_eq!(
                    git_text(repo, &["status", "--porcelain"]).unwrap(),
                    porcelain,
                    "squash={squash} {dirt}"
                );
                assert!(
                    wt.path().exists() && branch_exists(repo, "feature-x"),
                    "squash={squash} {dirt}"
                );
            }
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

    fn adv_with_base_record(content: &[u8]) -> Result<Option<(String, String)>, CommandError> {
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
                candidate,
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
                candidate,
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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
        // The object-database path never starts a merge, so a conflict has
        // nothing to roll back and the checkout stays idle.
        let (dir, wt) = conflicting_fixture();
        let repo = dir.path();
        let mut asked: Vec<String> = Vec::new();
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                asked.push(argv.join(" "));
                Ok(())
            },
        )
        .unwrap_err();
        let MergeTeardownError::Conflict {
            rolled_back,
            candidate,
            ..
        } = refused
        else {
            panic!("{refused:?}");
        };
        assert!(rolled_back);
        assert!(candidate.is_some(), "the conflict names the merge tree");
        assert!(
            asked.iter().any(|a| a.starts_with("merge-tree ")),
            "{asked:?}"
        );
        assert!(
            asked
                .iter()
                .all(|a| a != "merge --abort" && a != "reset --merge"),
            "an object-database conflict rolled the checkout back: {asked:?}"
        );
        main_checkout_is_idle(repo);

        // The in-checkout fallback still puts the rollback to the gate, and a
        // refusal is honoured: the command is asked about, never run, and the
        // error says why. Main stays parked.
        for squash in [false, true] {
            let (dir, wt) = conflicting_fixture();
            let repo = dir.path();
            let mut asked: Vec<String> = Vec::new();
            let refused = merge_and_teardown_configured(
                repo.to_str().unwrap(),
                wt.path().to_str().unwrap(),
                Some("main"),
                squash,
                None,
                crate::engine::merge_tree::IntegrateConfig {
                    require_write_tree: false,
                    write_tree: crate::engine::merge_tree::WriteTreeMode::ForceUnsupported,
                },
                None,
                &mut |argv: &[&str]| {
                    asked.push(argv.join(" "));
                    if argv.contains(&"--abort") || argv.contains(&"reset") {
                        Err("policy: no aborts".into())
                    } else {
                        Ok(())
                    }
                },
                &mut |_| {},
                &mut delete_dir_tree,
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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
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
        assert!(!refused.message().is_empty());
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
                Err("injected: the record check failed".into())
            },
            &mut |_, _| Err(std::io::Error::other("injected: put back failed")),
        );
        let why = held.err().expect("the rename path reports the failure");
        assert!(
            why.contains("not swept") && why.contains("put back"),
            "{why}",
            why = why.message()
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
    ) -> (Result<WorktreeRemoval, CommandError>, Vec<PathBuf>) {
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

    fn list_holding(holding: &Path) -> EntryListing {
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
    }

    /// 1. The aside is deleted by another sweep between this sweep's listing
    ///    and its delete: the delete says NotFound, which is the goal state.
    #[test]
    fn the_sweep_does_not_report_an_aside_a_concurrent_sweep_deleted_first() {
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-35-1");
        let report = sweep_teardown_asides_from(dir.path(), &list_holding, &|path| {
            std::fs::remove_dir_all(path)?; // the other sweep wins
            std::fs::remove_dir_all(path) // this sweep's delete finds nothing
        });
        assert!(report.problems.is_empty(), "{report:?}");
        assert!(!aside.exists());
    }

    /// 1. The other sweep deletes children while this one walks the tree, so
    ///    NotFound surfaces from inside the recursive delete.
    #[test]
    fn deleting_an_aside_survives_children_vanishing_mid_walk() {
        let dir = tempfile::tempdir().unwrap();
        let aside = dir.path().join("1-36-1");
        std::fs::create_dir_all(aside.join("build")).unwrap();
        std::fs::write(aside.join("build/out.o"), "object\n").unwrap();
        let attempts = std::cell::Cell::new(0);
        let result = delete_dir_tree_with(&aside, &|path| {
            attempts.set(attempts.get() + 1);
            if attempts.get() == 1 {
                // The other sweep removes a child, then this walk trips on it.
                std::fs::remove_file(path.join("build/out.o")).unwrap();
                return Err(std::io::Error::from(std::io::ErrorKind::NotFound));
            }
            std::fs::remove_dir_all(path)
        });
        assert!(result.is_ok(), "{result:?}");
        assert!(!aside.exists());
        assert_eq!(attempts.get(), 2);
    }

    /// 1. Only NotFound is the goal state: a delete that is refused is still
    ///    reported.
    #[cfg(unix)]
    #[test]
    fn the_sweep_still_reports_an_aside_it_is_not_permitted_to_delete() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, _wt) = teardown_fixture(None);
        let aside = plant_aside(dir.path(), "1-37-1");
        let holding = aside.parent().unwrap().to_path_buf();
        std::fs::set_permissions(&holding, std::fs::Permissions::from_mode(0o500)).unwrap();
        let report = sweep_teardown_asides_from(dir.path(), &list_holding, &delete_dir_tree);
        std::fs::set_permissions(&holding, std::fs::Permissions::from_mode(0o700)).unwrap();
        if report.removed == 1 {
            return; // running as a user the mode does not bind (root)
        }
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(
            report.problems[0].contains("could not delete the leftover"),
            "{report:?}"
        );
        assert!(aside.exists());
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

    /// One `hdiutil` call. A healthy one takes seconds; one that wedges (the
    /// image helper, a busy volume on detach) would otherwise park the suite.
    #[cfg(target_os = "macos")]
    const HDIUTIL_BUDGET: Duration = Duration::from_secs(60);

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
            // Built before the first hdiutil call so a call that wedges or
            // dies after mounting still reaches `Drop` and its detach.
            let mut volume = Self { mount, _dir: dir };
            let run = |args: &[&std::ffi::OsStr]| {
                let mut cmd = std::process::Command::new("hdiutil");
                cmd.args(args);
                let out = crate::test_support::run_within(
                    cmd,
                    "hdiutil",
                    crate::test_support::coverage_relaxed(HDIUTIL_BUDGET),
                );
                assert!(
                    out.success,
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
                volume.mount.as_os_str(),
                image.as_os_str(),
            ]);
            volume.mount = volume.mount.canonicalize().unwrap();
            volume
        }
    }

    #[cfg(target_os = "macos")]
    impl Drop for DiskImage {
        fn drop(&mut self) {
            // Never panics: this also runs while another failure unwinds.
            let mut cmd = std::process::Command::new("hdiutil");
            cmd.args(["detach", "-force"]).arg(&self.mount);
            let _ = crate::engine::git_cli::run_bounded(
                cmd,
                "hdiutil detach",
                crate::test_support::coverage_relaxed(HDIUTIL_BUDGET),
                None,
            );
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
        let report = sweep_teardown_asides_from(
            dir.path(),
            &|_| {
                Ok(vec![
                    Err(std::io::Error::other("injected: entry unreadable")),
                    Ok(("1-34-1".to_string(), path.clone())),
                ])
            },
            &delete_dir_tree,
        );
        assert_eq!(report.removed, 1, "{report:?}");
        assert_eq!(report.problems.len(), 1, "{report:?}");
        assert!(
            report.problems[0].contains("entry unreadable"),
            "{report:?}"
        );
        assert!(!aside.exists());
    }

    // ---- CARD 20 adversarial review ------------------------------------

    /// Primary on `develop` (the remote's default), `main` and `master` as
    /// worktree branches that collide with the old guess list, a detached
    /// worktree, and an ordinary lane.
    fn adv_develop_repo() -> (tempfile::TempDir, tempfile::TempDir, PathBuf, String) {
        let (main, parent, _) = crate::test_support::repo_with_worktrees(0);
        let root = plain(main.path());
        git_in(&root, &["checkout", "-q", "-b", "develop"]);
        git_in(
            &root,
            &["commit", "-q", "--allow-empty", "-m", "develop moves"],
        );
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
        (main, parent, root, develop)
    }

    /// `git worktree add -q <flags> <path> <tail>`, then `commits` commits in it.
    fn adv_add(
        root: &Path,
        parent: &Path,
        name: &str,
        flags: &[&str],
        tail: &[&str],
        commits: usize,
    ) -> PathBuf {
        let path = parent.join(name);
        let mut args = vec!["worktree", "add", "-q"];
        args.extend_from_slice(flags);
        args.push(path.to_str().unwrap());
        args.extend_from_slice(tail);
        git_in(root, &args);
        crate::test_support::trust_repo(&path);
        let path = plain(&path);
        for n in 0..commits {
            std::fs::write(path.join(format!("{name}-{n}.txt")), "x").unwrap();
            git_in(&path, &["add", "."]);
            git_in(&path, &["commit", "-q", "-m", &format!("{name} {n}")]);
        }
        path
    }

    fn adv_measured(list: &[WorktreeInfo], branch: &str) -> Option<(usize, usize)> {
        list.iter()
            .find(|w| w.branch.as_deref() == Some(branch))
            .unwrap_or_else(|| panic!("{branch} not listed"))
            .main_divergence
            .clone()
            .map(|d| (d.ahead, d.behind))
    }

    /// Attack 3: batched, walked and list_branches must agree for every
    /// branch, including names that collide with main/master.
    #[test]
    fn adv_divergence_parity_with_colliding_names_detached_and_default() {
        let (main, parent, root, develop) = adv_develop_repo();
        let _keep = (&main, &parent);
        adv_add(&root, parent.path(), "wt-main", &[], &["main"], 1);
        adv_add(
            &root,
            parent.path(),
            "wt-master",
            &["-b", "master"],
            &["develop"],
            2,
        );
        adv_add(
            &root,
            parent.path(),
            "wt-lane",
            &["-b", "feat/x"],
            &["develop"],
            1,
        );
        adv_add(
            &root,
            parent.path(),
            "wt-det",
            &["--detach"],
            &["develop"],
            0,
        );
        let repo = root.to_str().unwrap();
        let batched = list_worktrees(repo).expect("list");
        let format = format!("--format=%(refname)%00%(ahead-behind:{develop})");
        let walked =
            crate::engine::git_cli::with_forced_spawn_failure_of(&format, || list_worktrees(repo))
                .expect("list");
        let branches = crate::engine::git_reader::GitReader::list_branches(repo).expect("branches");
        for name in ["main", "master", "feat/x"] {
            let b = branches
                .iter()
                .find(|b| b.name == name && !b.is_remote)
                .unwrap();
            let expected = Some((b.commits_ahead_of_base, b.commits_behind_base));
            assert_eq!(adv_measured(&batched, name), expected, "batched {name}");
            assert_eq!(adv_measured(&walked, name), expected, "walked {name}");
        }
        assert_eq!(
            adv_measured(&batched, "develop"),
            Some((0, 0)),
            "default itself"
        );
        assert_eq!(
            adv_measured(&walked, "develop"),
            Some((0, 0)),
            "default itself, walked"
        );
        for list in [&batched, &walked] {
            let detached = list
                .iter()
                .find(|w| w.branch.is_none())
                .expect("detached listed");
            assert_eq!(detached.main_divergence, None);
        }
    }

    /// Attack 3: an unborn-branch worktree and an unrelated history (the base
    /// is not an ancestor) in both paths.
    #[test]
    fn adv_divergence_parity_unborn_and_unrelated_history() {
        let (main, parent, root, develop) = adv_develop_repo();
        let _keep = (&main, &parent);
        let unborn = parent.path().join("wt-unborn");
        git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "--orphan",
                "-b",
                "unborn",
                unborn.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&unborn);
        let unrelated = parent.path().join("wt-unrelated");
        git_in(
            &root,
            &[
                "worktree",
                "add",
                "-q",
                "--orphan",
                "-b",
                "unrelated",
                unrelated.to_str().unwrap(),
            ],
        );
        crate::test_support::trust_repo(&unrelated);
        let unrelated = plain(&unrelated);
        std::fs::write(unrelated.join("u.txt"), "u").unwrap();
        git_in(&unrelated, &["add", "."]);
        git_in(&unrelated, &["commit", "-q", "-m", "unrelated root"]);
        let repo = root.to_str().unwrap();
        let batched = list_worktrees(repo).expect("list");
        let format = format!("--format=%(refname)%00%(ahead-behind:{develop})");
        let walked =
            crate::engine::git_cli::with_forced_spawn_failure_of(&format, || list_worktrees(repo))
                .expect("list");
        let branches = crate::engine::git_reader::GitReader::list_branches(repo).expect("branches");
        let b = branches.iter().find(|b| b.name == "unrelated").unwrap();
        let expected = Some((b.commits_ahead_of_base, b.commits_behind_base));
        assert_eq!(adv_measured(&batched, "unrelated"), expected, "batched");
        assert_eq!(adv_measured(&walked, "unrelated"), expected, "walked");
        assert_eq!(
            adv_measured(&batched, "unborn"),
            adv_measured(&walked, "unborn"),
            "unborn parity"
        );
    }

    /// Attack 3: a tag that shares a lane's name. The batched path reads
    /// `refs/heads/<lane>`; the fallback hands the bare name to rev-list,
    /// where a tag wins.
    #[test]
    fn adv_divergence_fallback_reads_the_branch_not_a_same_named_tag() {
        let (main, parent, root, develop) = adv_develop_repo();
        let _keep = (&main, &parent);
        adv_add(
            &root,
            parent.path(),
            "wt-lane",
            &["-b", "lane"],
            &["develop"],
            2,
        );
        git_in(&root, &["tag", "lane", "main"]);
        let repo = root.to_str().unwrap();
        let batched = list_worktrees(repo).expect("list");
        let format = format!("--format=%(refname)%00%(ahead-behind:{develop})");
        let walked =
            crate::engine::git_cli::with_forced_spawn_failure_of(&format, || list_worktrees(repo))
                .expect("list");
        assert_eq!(adv_measured(&batched, "lane"), Some((2, 0)), "batched");
        assert_eq!(
            adv_measured(&walked, "lane"),
            Some((2, 0)),
            "the rev-list fallback measured the tag, not the branch"
        );
    }

    /// Attack 5: fifty linked worktrees cost one default-base lookup, and a
    /// second listing with no racy window costs none.
    #[test]
    fn adv_fifty_worktrees_one_default_base_lookup_and_warm_costs_none() {
        let _kept = crate::engine::ref_cache::retention();
        let (main, parent, linked) = crate::test_support::repo_with_worktrees(50);
        let _keep = (&main, &parent);
        let root = plain(main.path());
        let count = |needle: &str| {
            crate::engine::git_cli::spawn_log::spawns_in(&root)
                .iter()
                .filter(|argv| argv.iter().any(|a| a == needle))
                .count()
        };
        crate::engine::ref_cache::eager(|| {
            let cold = list_worktrees(root.to_str().unwrap()).expect("list");
            assert_eq!(cold.len(), 51);
            assert_eq!(count("refs/remotes/origin/HEAD"), 1, "cold lookups");
            let spawned = crate::engine::git_cli::spawn_log::spawns_in(&root).len();
            let _ = list_worktrees(root.to_str().unwrap()).expect("list");
            assert_eq!(count("refs/remotes/origin/HEAD"), 1, "warm lookups");
            let again = crate::engine::git_cli::spawn_log::spawns_in(&root).len();
            eprintln!("root spawns cold={spawned} warm-extra={}", again - spawned);
        });
        let _ = linked;
    }

    /// Attack 4: the default is a remote-tracking ref with no local head, and
    /// the primary's HEAD is unborn on that name. Nothing may be touched.
    #[test]
    fn adv_null_target_with_a_remote_only_default_touches_nothing() {
        let (dir, wt) = teardown_fixture_on(None, "main");
        let repo = dir.path();
        git_in(
            repo,
            &["remote", "add", "origin", "https://example.com/r.git"],
        );
        git_in(repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        git_in(
            repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        git_in(repo, &["branch", "-m", "main", "work"]);
        git_in(repo, &["symbolic-ref", "HEAD", "refs/heads/main"]);
        let work_tip = rev(repo, "work");
        let result = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            None,
            false,
            None,
            &mut allow_all,
        );
        let refused = result.expect_err("a remote-only default must not be merged into");
        eprintln!("refusal: {refused:?}");
        assert!(wt.path().exists(), "the worktree was removed");
        assert!(
            branch_exists(repo, "feature-x"),
            "the lane branch was deleted"
        );
        assert_eq!(rev(repo, "work"), work_tip, "work moved");
        assert!(
            !branch_exists(repo, "main"),
            "a local main was created from the merge"
        );
        assert!(!repo.join("feature.txt").exists());
    }

    /// Attack 4: the default is the lane's own branch.
    #[test]
    fn adv_null_target_equal_to_the_lanes_own_branch_is_refused() {
        let (dir, wt) = teardown_fixture_on(None, "main");
        let repo = dir.path();
        git_in(
            repo,
            &["remote", "add", "origin", "https://example.com/r.git"],
        );
        git_in(
            repo,
            &["update-ref", "refs/remotes/origin/feature-x", "feature-x"],
        );
        git_in(
            repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/feature-x",
            ],
        );
        let main_tip = rev(repo, "main");
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            None,
            false,
            None,
            &mut allow_all,
        )
        .expect_err("merging a lane into itself");
        eprintln!("refusal: {refused:?}");
        assert!(wt.path().exists() && branch_exists(repo, "feature-x"));
        assert_eq!(rev(repo, "main"), main_tip);
    }

    /// Attack 4: the default moves after it was resolved (the gate runs the
    /// move when the merge starts). The merge must stay coherent: it lands in
    /// the branch it resolved and reports that branch.
    #[test]
    fn adv_default_moving_after_resolution_does_not_split_the_merge() {
        let (dir, wt) = teardown_fixture_on(None, "main");
        let repo = dir.path().to_path_buf();
        git_in(
            &repo,
            &["remote", "add", "origin", "https://example.com/r.git"],
        );
        git_in(&repo, &["branch", "develop"]);
        git_in(&repo, &["update-ref", "refs/remotes/origin/main", "main"]);
        git_in(
            &repo,
            &["update-ref", "refs/remotes/origin/develop", "develop"],
        );
        git_in(
            &repo,
            &[
                "symbolic-ref",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ],
        );
        let mut moved = false;
        let mut gate = |argv: &[&str]| -> Result<(), CommandError> {
            if !moved && matches!(argv.first(), Some(&"merge" | &"merge-tree" | &"update-ref")) {
                moved = true;
                git_in(
                    &repo,
                    &[
                        "symbolic-ref",
                        "refs/remotes/origin/HEAD",
                        "refs/remotes/origin/develop",
                    ],
                );
            }
            Ok(())
        };
        let merged = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            wt.path().to_str().unwrap(),
            None,
            false,
            None,
            &mut gate,
        )
        .expect("merged");
        assert_eq!(merged.target_branch, "main");
        assert!(repo.join("feature.txt").exists());
    }

    // ---- adversarial review: aside deletion (ft-22bcf053) ----

    fn vanishing(_: &Path) -> std::io::Result<()> {
        Err(std::io::Error::from(std::io::ErrorKind::NotFound))
    }

    /// A symlink inside an aside, pointing outside it, is unlinked and never
    /// followed.
    #[cfg(unix)]
    #[test]
    fn adv22_a_symlink_inside_an_aside_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("precious"), "keep").unwrap();
        let dir = tempfile::tempdir().unwrap();
        let aside = dir.path().join("1-50-1");
        std::fs::create_dir_all(aside.join("build")).unwrap();
        std::os::unix::fs::symlink(outside.path(), aside.join("build/link")).unwrap();
        delete_dir_tree(&aside).unwrap();
        assert!(!aside.exists());
        assert!(outside.path().join("precious").exists());
    }

    /// TOCTOU: the aside is swapped for a symlink to a real directory after the
    /// sweep checked it and before it deletes. The target must survive.
    #[cfg(unix)]
    #[test]
    fn adv22_an_aside_swapped_for_a_symlink_does_not_delete_the_target() {
        let (dir, _wt) = teardown_fixture(None);
        let victim = tempfile::tempdir().unwrap();
        std::fs::write(victim.path().join("precious"), "keep").unwrap();
        plant_aside(dir.path(), "1-51-1");
        let victim_path = victim.path().to_path_buf();
        let _ = sweep_teardown_asides_from(dir.path(), &list_holding, &|path| {
            std::fs::remove_dir_all(path)?;
            std::os::unix::fs::symlink(&victim_path, path)?;
            delete_dir_tree(path)
        });
        assert!(
            victim.path().join("precious").exists(),
            "the symlink target was deleted"
        );
    }

    /// A walk that keeps vanishing with the tree still there is retried a
    /// bounded number of times and then returned as an error.
    #[test]
    fn adv22_a_walk_that_keeps_vanishing_is_bounded_and_returns_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let aside = dir.path().join("1-52-1");
        std::fs::create_dir_all(&aside).unwrap();
        let attempts = std::cell::Cell::new(0usize);
        let result = delete_dir_tree_with(&aside, &|_| {
            attempts.set(attempts.get() + 1);
            vanishing(&aside)
        });
        assert!(result.is_err(), "{result:?}");
        assert_eq!(attempts.get(), VANISHED_WALK_RETRIES + 1);
        assert!(aside.exists());
    }

    /// Permission-denied on a nested child is returned, not swallowed.
    #[cfg(unix)]
    #[test]
    fn adv22_permission_denied_on_a_nested_child_is_reported() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let aside = dir.path().join("1-53-1");
        std::fs::create_dir_all(aside.join("a/b")).unwrap();
        std::fs::write(aside.join("a/b/f"), "x").unwrap();
        std::fs::set_permissions(aside.join("a/b"), std::fs::Permissions::from_mode(0o500))
            .unwrap();
        let result = delete_dir_tree(&aside);
        std::fs::set_permissions(aside.join("a/b"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        if result.is_ok() {
            return; // running as a user the mode does not bind (root)
        }
        assert_eq!(
            result.unwrap_err().kind(),
            std::io::ErrorKind::PermissionDenied
        );
        assert!(aside.exists());
    }

    /// The sweep must not turn a persistent in-walk NotFound (tree still
    /// there) into "already removed": it is a problem, and the marker, the
    /// only thing that finds a sibling aside, must stay.
    #[test]
    fn adv22_sweep_reports_a_persistent_vanishing_walk_and_keeps_the_marker() {
        let (dir, wt) = teardown_fixture(None);
        let beside = wt
            .path()
            .parent()
            .unwrap()
            .join(".lane.gitpulse-teardown-1-54-1");
        std::fs::create_dir_all(beside.join("build")).unwrap();
        let holding = teardown_holding_dir(dir.path());
        std::fs::create_dir_all(&holding).unwrap();
        let marker = holding.join("1-54-1.sibling");
        std::fs::write(&marker, format!("{}\n{}", beside.display(), "feed02")).unwrap();
        std::fs::write(
            beside.join(SENTINEL_FILE),
            format!(
                "{}\n{}",
                resolve_git_common_dir(dir.path()).unwrap().display(),
                "feed02"
            ),
        )
        .unwrap();
        let report = sweep_teardown_asides_from(dir.path(), &list_holding, &|path| {
            delete_dir_tree_with(path, &vanishing)
        });
        assert!(beside.exists());
        assert!(
            marker.exists(),
            "the marker was removed while the aside is still there: {report:?}"
        );
        assert_eq!(
            report.problems.len(),
            1,
            "a persistent failure went unreported: {report:?}"
        );
    }

    /// finish_teardown keeps its contract: a delete that cannot finish is
    /// reported to the caller, and the marker stays.
    #[test]
    fn adv22_finish_teardown_reports_a_persistent_vanishing_walk() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("1-55-1");
        std::fs::create_dir_all(&path).unwrap();
        let marker = dir.path().join("1-55-1.sibling");
        std::fs::write(&marker, "m").unwrap();
        let aside = TeardownAside {
            path: path.clone(),
            marker: Some(marker.clone()),
        };
        let note = finish_teardown(Some(aside), &mut |p| delete_dir_tree_with(p, &vanishing));
        assert!(note.is_some_and(|n| n.contains("could not be deleted")));
        assert!(marker.exists());
        assert!(path.exists());
    }

    /// finish_teardown with a path already gone (a sweep won) is clean.
    #[test]
    fn adv22_finish_teardown_of_an_already_deleted_aside_is_clean() {
        let dir = tempfile::tempdir().unwrap();
        let aside = TeardownAside {
            path: dir.path().join("nope"),
            marker: None,
        };
        assert_eq!(finish_teardown(Some(aside), &mut delete_dir_tree), None);
    }

    fn small_sparse_fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let sandbox = tempfile::tempdir().unwrap();
        let anchor = sandbox.path().join("mono").canonicalize_or_create();
        std::fs::create_dir_all(anchor.join("src")).unwrap();
        std::fs::write(anchor.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
        git_in(&anchor, &["init", "-q", "-b", "main"]);
        git_in(&anchor, &["config", "user.name", "Sparse Test"]);
        git_in(&anchor, &["config", "user.email", "sparse@example.com"]);
        git_in(&anchor, &["add", "."]);
        git_in(&anchor, &["commit", "-q", "-m", "init"]);
        let target = sandbox.path().join("lane");
        (sandbox, anchor, target)
    }

    trait CanonicalizeOrCreate {
        fn canonicalize_or_create(self) -> PathBuf;
    }

    impl CanonicalizeOrCreate for PathBuf {
        fn canonicalize_or_create(self) -> PathBuf {
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    /// Every git write of a sparse creation is judged: the add, sparse-checkout
    /// init/set and the checkout, and none of them is `git config`.
    #[test]
    fn a_sparse_creation_hands_every_git_write_to_the_gate_and_writes_no_config() {
        let (_sandbox, anchor, target) = small_sparse_fixture();
        let mut judged: Vec<Vec<String>> = Vec::new();
        let created = add_sparse_worktree_zero_copy(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            "agent/judged",
            None,
            &["src".to_string()],
            false,
            &mut |args| {
                judged.push(args.iter().map(|a| a.to_string()).collect());
                Ok(())
            },
        )
        .expect("sparse worktree");
        let heads: Vec<&str> = judged.iter().map(|a| a[0].as_str()).collect();
        assert_eq!(
            heads,
            ["worktree", "sparse-checkout", "sparse-checkout", "checkout"],
            "{judged:?}"
        );
        assert!(judged.iter().all(|a| a[0] != "config"), "{judged:?}");
        assert!(Path::new(&created.path).join("src/lib.rs").is_file());
    }

    /// The owner's one allowance under ft-7450105d: git's own sparse-checkout
    /// may add `extensions.worktreeConfig = true` to the shared config. Nothing
    /// else may change there; the cone itself lives in the lane's
    /// `config.worktree`.
    #[test]
    fn a_sparse_creation_changes_the_shared_config_by_worktree_config_only() {
        let (_sandbox, anchor, target) = small_sparse_fixture();
        let shared = anchor.join(".git/config");
        let lines = |text: String| -> std::collections::BTreeSet<String> {
            text.lines()
                .map(|l| l.trim().to_string())
                .filter(|l| !l.is_empty())
                .collect()
        };
        let before = lines(std::fs::read_to_string(&shared).unwrap());
        add_sparse_worktree_zero_copy(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            "agent/shared-config",
            None,
            &["src".to_string()],
            false,
            &mut |_| Ok(()),
        )
        .expect("sparse worktree");
        let after = lines(std::fs::read_to_string(&shared).unwrap());
        let added: Vec<&String> = after.difference(&before).collect();
        let removed: Vec<&String> = before.difference(&after).collect();
        assert_eq!(
            added,
            ["[extensions]", "worktreeConfig = true"],
            "{after:?}"
        );
        assert!(removed.is_empty(), "{removed:?}");
    }

    /// A refused write stops the sequence, and the undo is judged too: a
    /// refusal of the first sparse step leaves no half-made worktree or branch.
    #[test]
    fn a_refused_sparse_step_stops_the_sequence_and_rolls_back_through_the_gate() {
        let (_sandbox, anchor, target) = small_sparse_fixture();
        let mut judged: Vec<String> = Vec::new();
        let err = add_sparse_worktree_zero_copy(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            "agent/refused",
            None,
            &["src".to_string()],
            false,
            &mut |args| {
                judged.push(args[0].to_string());
                if args[0] == "sparse-checkout" {
                    return Err(CommandError::plain("refused by policy"));
                }
                Ok(())
            },
        )
        .expect_err("a refusal is an error");
        assert!(
            err.message().contains("refused by policy"),
            "{}",
            err.message()
        );
        assert_eq!(
            judged,
            ["worktree", "sparse-checkout", "worktree", "branch"]
        );
        assert!(!target.exists(), "the worktree was rolled back");
        let branches = git_text(&anchor, &["branch", "--list", "agent/refused"]).unwrap();
        assert!(
            branches.trim().is_empty(),
            "the branch was deleted: {branches}"
        );
    }

    /// When the undo itself is refused, the error says what is left behind.
    #[test]
    fn a_refused_rollback_names_what_it_left_behind() {
        let (_sandbox, anchor, target) = small_sparse_fixture();
        let err = add_sparse_worktree_zero_copy(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            "agent/stuck",
            None,
            &["src".to_string()],
            false,
            &mut |args| {
                if args[0] == "sparse-checkout" || args[0] == "branch" {
                    return Err(CommandError::plain("refused"));
                }
                Ok(())
            },
        )
        .expect_err("refused");
        assert!(
            err.message().contains("rollback incomplete"),
            "{}",
            err.message()
        );
        assert!(
            err.message().contains("branch agent/stuck was not deleted"),
            "{}",
            err.message()
        );
    }

    /// A caller that did not ask for a DevMap store pays nothing for one, and
    /// the result says so; one that asks is told what happened.
    #[test]
    fn a_worktree_created_without_asking_seeds_no_devmap_store_and_says_so() {
        let (_sandbox, anchor, target) = small_sparse_fixture();
        let created = add_worktree_cached(
            anchor.to_str().unwrap(),
            target.to_str().unwrap(),
            Some("agent/plain"),
            None,
            false,
            None,
            false,
        )
        .expect("worktree");
        assert_eq!(created.devmap, DevmapOutcome::NotRequested);
        assert!(!devmap_query::paths::store_path(Path::new(&created.path)).exists());

        let asked = add_worktree_with_devmap(
            anchor.to_str().unwrap(),
            anchor.parent().unwrap().join("lane2").to_str().unwrap(),
            Some("agent/asked"),
            None,
            false,
            None,
            false,
            DevmapMode::Copy,
        )
        .expect("worktree");
        assert_ne!(
            asked.devmap,
            DevmapOutcome::NotRequested,
            "an asked-for seeding reports an outcome"
        );
    }

    #[test]
    fn test_sparse_worktree_zero_copy_expands_devmap_deps_and_skips_unrelated_dirs() {
        let sandbox = tempfile::tempdir().unwrap();
        let anchor = sandbox.path().join("mono");
        std::fs::create_dir_all(anchor.join("crates/api/src")).unwrap();
        std::fs::create_dir_all(anchor.join("crates/shared/src")).unwrap();
        std::fs::create_dir_all(anchor.join("crates/unrelated_heavy/src")).unwrap();
        std::fs::create_dir_all(anchor.join("crates/api/node_modules/pkg")).unwrap();
        std::fs::create_dir_all(anchor.join("crates/unrelated_heavy/node_modules/pkg")).unwrap();

        std::fs::write(anchor.join(".gitignore"), "**/node_modules/\n.devmap/\n").unwrap();
        std::fs::write(anchor.join("Cargo.toml"), "[workspace]\n").unwrap();
        std::fs::write(
            anchor.join("crates/api/src/lib.rs"),
            "pub fn handle() { shared_helper(); }\n",
        )
        .unwrap();
        std::fs::write(
            anchor.join("crates/shared/src/lib.rs"),
            "pub fn shared_helper() {}\n",
        )
        .unwrap();
        std::fs::write(
            anchor.join("crates/unrelated_heavy/src/lib.rs"),
            "pub fn heavy_unused() {}\n",
        )
        .unwrap();
        std::fs::write(
            anchor.join("crates/api/node_modules/pkg/index.js"),
            "module.exports = 'api';\n",
        )
        .unwrap();
        std::fs::write(
            anchor.join("crates/unrelated_heavy/node_modules/pkg/index.js"),
            "module.exports = 'heavy';\n",
        )
        .unwrap();

        git_in(&anchor, &["init", "-q", "-b", "main"]);
        git_in(&anchor, &["config", "user.name", "Sparse Test"]);
        git_in(&anchor, &["config", "user.email", "sparse@example.com"]);
        git_in(&anchor, &["add", "."]);
        git_in(&anchor, &["commit", "-q", "-m", "init monorepo"]);
        crate::test_support::trust_repo(&anchor);

        // Seed a real DevMap SQLite store in `anchor` with an outbound edge:
        // crates/api/src/lib.rs -> crates/shared/src/lib.rs
        let canon_anchor = anchor.canonicalize().unwrap();
        let db_path = devmap_query::paths::store_path(&canon_anchor);
        std::fs::create_dir_all(db_path.parent().unwrap()).unwrap();
        let store = devmap_store::Store::open(&db_path).unwrap();
        drop(store);

        let head_sha = rev(&canon_anchor, "HEAD");
        crate::devmap::ensure_state_dir_excluded(&canon_anchor);
        let scanned = devmap_extract::scan_tree(&canon_anchor).unwrap();
        let mut hashes: std::collections::BTreeMap<String, u64> = scanned
            .file_hashes()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect();
        let conn = rusqlite::Connection::open(&db_path).unwrap();
        conn.execute(
            "UPDATE pending_state SET repo_root = ?1 WHERE singleton = 1",
            [canon_anchor.to_str().unwrap()],
        )
        .unwrap();
        let ordered_files = [
            "crates/api/src/lib.rs",
            "crates/shared/src/lib.rs",
            "crates/unrelated_heavy/src/lib.rs",
        ];
        let mut all_files: Vec<(String, u64)> = Vec::new();
        for p in ordered_files {
            let h = hashes.remove(p).expect("scanned crate file");
            all_files.push((p.to_string(), h));
        }
        for (p, h) in hashes {
            all_files.push((p, h));
        }
        for (idx, (path, raw_hash)) in all_files.iter().enumerate() {
            let id = (idx + 1) as i64;
            let lang = devmap_extract::detect_language(Path::new(path));
            let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity(lang);
            let engine_json =
                serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
                    grammar: lang.to_string(),
                    grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
                })
                .unwrap();
            let extraction_json = serde_json::json!({
                "file_path": path,
                "language": lang,
                "content_hash": *raw_hash,
                "parse_outcome": "Clean",
                "symbols": [],
                "imports": [],
                "calls": []
            })
            .to_string();
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .unwrap();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, ?3, '\"Clean\"', ?4, ?5, '[]', ?6, ?7)",
                (
                    id,
                    *raw_hash as i64,
                    lang,
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
        }
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                &head_sha,
                r#"{"total_files":3,"total_symbols":3,"total_edges":1,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
                canon_anchor.to_str().unwrap(),
            ),
        )
        .unwrap();
        conn.execute(
            "INSERT INTO edge_rows
               (edge_id, source_file_id, target_file_id,
                source_symbol, target_symbol, edge_kind, confidence,
                valid_from, valid_to)
             VALUES (0, 1, 2, 'handle', 'shared_helper', 'Calls', 0.95, 1, NULL)",
            [],
        )
        .unwrap();
        drop(conn);

        let wt_target = sandbox.path().join("lane-sparse-api");
        let created = add_sparse_worktree_zero_copy(
            canon_anchor.to_str().unwrap(),
            wt_target.to_str().unwrap(),
            "agent/sparse-api",
            None,
            &["crates/api".to_string()],
            true,
            &mut |_| Ok(()),
        )
        .expect("sparse zero-copy worktree");

        assert!(created.zero_copy_devmap, "{created:?}");
        assert!(
            created
                .materialized_cone_dirs
                .iter()
                .any(|d| d.starts_with("crates/api")),
            "{created:?}"
        );
        assert!(
            created
                .materialized_cone_dirs
                .iter()
                .any(|d| d.starts_with("crates/shared")),
            "DevMap dependency crates/shared must be auto-expanded into sparse cone: {created:?}"
        );
        assert!(
            !created
                .materialized_cone_dirs
                .iter()
                .any(|d| d.contains("unrelated_heavy")),
            "unrelated subsystem must NOT be in sparse cone: {created:?}"
        );

        // Verify disk materialization: root + api + shared exist; unrelated_heavy is NOT copied!
        assert!(wt_target.join("Cargo.toml").is_file());
        assert!(wt_target.join("crates/api/src/lib.rs").is_file());
        assert!(wt_target.join("crates/shared/src/lib.rs").is_file());
        assert!(
            !wt_target.join("crates/unrelated_heavy").exists(),
            "unrelated_heavy must NOT be materialized on disk in sparse worktree"
        );
        // Main checkout remains 100% intact (per-worktree sparse config isolation).
        assert!(canon_anchor
            .join("crates/unrelated_heavy/src/lib.rs")
            .is_file());

        // Selective cache cloning copied `crates/api/node_modules` and NOT `crates/unrelated_heavy/node_modules`!
        assert!(wt_target
            .join("crates/api/node_modules/pkg/index.js")
            .is_file());
        assert!(!wt_target
            .join("crates/unrelated_heavy/node_modules")
            .exists());

        // Zero-copy DevMap check: no devmap.sqlite or delta.sqlite copied; stack_lease.json exists!
        assert!(!devmap_query::paths::store_path(&wt_target).exists());
        assert!(!devmap_query::paths::stack_delta_path(&wt_target).exists());
        crate::test_support::trust_repo(&wt_target);
        let lease = devmap_query::stacked::StackLease::read_from(
            &devmap_query::paths::stack_lease_path(&wt_target),
        )
        .expect("stack_lease.json present");
        assert_eq!(lease.generation_id, 1);

        let (dirty, truncated) = user_changed_paths(wt_target.to_str().unwrap()).unwrap();
        assert!(dirty.is_empty() && !truncated, "dirty={dirty:?}");
    }

    #[test]
    fn test_sparse_worktree_rejects_traversal_and_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        assert!(expand_sparse_cone_with_devmap(dir.path(), &[]).is_err());
        assert!(expand_sparse_cone_with_devmap(dir.path(), &["../secret".into()]).is_err());
        assert!(expand_sparse_cone_with_devmap(dir.path(), &["/etc/passwd".into()]).is_err());
        assert!(expand_sparse_cone_with_devmap(dir.path(), &["-bad-flag".into()]).is_err());
    }

    #[test]
    fn test_sparse_worktree_dotted_dirs_chained_zero_copy_and_lane_target_refusal() {
        let sandbox = tempfile::tempdir().unwrap();
        let anchor = sandbox.path().join("anchor");
        std::fs::create_dir_all(&anchor).unwrap();
        let run_git = |cwd: &Path, args: &[&str]| {
            let out = std::process::Command::new("git")
                .current_dir(cwd)
                .args(args)
                .output_locked()
                .unwrap();
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            String::from_utf8_lossy(&out.stdout).trim().to_string()
        };

        run_git(&anchor, &["init", "-q", "-b", "main"]);
        run_git(&anchor, &["config", "user.name", "Test"]);
        run_git(&anchor, &["config", "user.email", "test@example.com"]);

        std::fs::create_dir_all(anchor.join(".github/workflows")).unwrap();
        std::fs::create_dir_all(anchor.join("releases/v1.0")).unwrap();
        std::fs::create_dir_all(anchor.join("heavy_unwanted")).unwrap();
        std::fs::write(anchor.join(".github/workflows/ci.yml"), "name: CI\n").unwrap();
        std::fs::write(anchor.join("releases/v1.0/notes.txt"), "v1.0\n").unwrap();
        std::fs::write(anchor.join("heavy_unwanted/blob.rs"), "pub fn big() {}\n").unwrap();
        run_git(&anchor, &["add", "."]);
        run_git(&anchor, &["commit", "-q", "-m", "init dotted dirs"]);

        let canon_anchor = std::fs::canonicalize(&anchor).unwrap();
        crate::test_support::trust_repo(&canon_anchor);
        crate::devmap::ensure_state_dir_excluded(&canon_anchor);

        let head_sha = run_git(&canon_anchor, &["rev-parse", "HEAD"]);
        let scanned = devmap_extract::scan_tree(&canon_anchor).unwrap();
        let db = devmap_query::paths::store_path(&canon_anchor);
        std::fs::create_dir_all(db.parent().unwrap()).unwrap();
        let store = devmap_store::Store::open(&db).unwrap();
        store.bind_repo_root(&canon_anchor).unwrap();
        drop(store);
        let conn = rusqlite::Connection::open(&db).unwrap();
        for (idx, (path, raw_hash)) in scanned.file_hashes().into_iter().enumerate() {
            let id = (idx + 1) as i64;
            let lang = devmap_extract::detect_language(Path::new(path));
            let (grammar_ver, analyzer_ver) = devmap_extract::cache::current_payload_identity(lang);
            let engine_json =
                serde_json::to_string(&devmap_extract::ExtractionEngine::TreeSitter {
                    grammar: lang.to_string(),
                    grammar_version: grammar_ver.parse::<u32>().unwrap_or(1),
                })
                .unwrap();
            let extraction_json = serde_json::json!({
                "file_path": path,
                "language": lang,
                "content_hash": raw_hash,
                "parse_outcome": "Clean",
                "symbols": [],
                "imports": [],
                "calls": []
            })
            .to_string();
            conn.execute("INSERT INTO paths (id, path) VALUES (?1, ?2)", (id, path))
                .unwrap();
            conn.execute(
                "INSERT INTO file_payloads
                   (payload_id, file_id, content_hash, language,
                    parse_outcome_json, engine_json, extraction_json,
                    entry_wiring_json, grammar_version, analyzer_version)
                 VALUES (?1, ?1, ?2, ?3, '\"Clean\"', ?4, ?5, '[]', ?6, ?7)",
                (
                    id,
                    raw_hash as i64,
                    lang,
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
        }
        conn.execute(
            "INSERT INTO generations (id, created_at, head_sha, analysis_json, repo_root)
             VALUES (1, 0.0, ?1, ?2, ?3)",
            (
                &head_sha,
                r#"{"total_files":0,"total_symbols":0,"total_edges":0,"dead_symbols":[],"communities":[],"status":"Ok","unresolved_calls":0}"#,
                canon_anchor.to_str().unwrap(),
            ),
        )
        .unwrap();
        drop(conn);

        // 1. Refuse `.gitpulse/worktrees/...` targets in both `add_sparse_worktree_zero_copy`
        //    and `resolve_worktree_path_for_branch`.
        let illegal_lane_target = canon_anchor.join(".gitpulse/worktrees/illegal-lane");
        let sparse_refused = add_sparse_worktree_zero_copy(
            canon_anchor.to_str().unwrap(),
            illegal_lane_target.to_str().unwrap(),
            "agent/illegal",
            None,
            &[".github".to_string()],
            true,
            &mut |_| Ok(()),
        )
        .expect_err("must refuse .gitpulse/worktrees target");
        assert!(
            sparse_refused.message().contains(".gitpulse/worktrees"),
            "{sparse_refused:?}"
        );

        let resolved_refused = resolve_worktree_path_for_branch(
            canon_anchor.to_str().unwrap(),
            "agent/illegal",
            Some(illegal_lane_target.to_str().unwrap()),
        )
        .expect_err("resolve_worktree_path_for_branch must refuse .gitpulse/worktrees target");
        assert!(
            resolved_refused.message().contains(".gitpulse/worktrees"),
            "{resolved_refused:?}"
        );

        // 2. Top-level dotted directory `.github` must resolve to `.github` cone (not root `None`!).
        let wt1 = sandbox.path().join("wt1-github");
        let created1 = add_sparse_worktree_zero_copy(
            canon_anchor.to_str().unwrap(),
            wt1.to_str().unwrap(),
            "agent/wt1-github",
            None,
            &[".github".to_string()],
            true,
            &mut |_| Ok(()),
        )
        .expect("wt1 sparse .github");
        assert_eq!(created1.materialized_cone_dirs, vec![".github".to_string()]);
        assert!(wt1.join(".github/workflows/ci.yml").is_file());
        assert!(!wt1.join("releases/v1.0").exists());
        assert!(!wt1.join("heavy_unwanted").exists());

        // 3. Chaining `add_sparse_worktree_zero_copy` from `wt1` (a sparse zero-copy linked worktree
        //    where `releases/v1.0` is NOT on disk and `devmap.sqlite` is NOT local!) MUST resolve
        //    `releases/v1.0` via git tree object and chain zero-copy DevMap from the primary anchor!
        crate::test_support::trust_repo(&wt1);
        let wt2 = sandbox.path().join("wt2-releases");
        let created2 = add_sparse_worktree_zero_copy(
            wt1.to_str().unwrap(),
            wt2.to_str().unwrap(),
            "agent/wt2-releases",
            None,
            &["releases/v1.0".to_string()],
            true,
            &mut |_| Ok(()),
        )
        .expect("wt2 chained from wt1");
        assert_eq!(
            created2.materialized_cone_dirs,
            vec!["releases/v1.0".to_string()]
        );
        assert!(
            created2.zero_copy_devmap,
            "chained sparse worktree from zero-copy worktree must still get zero_copy_devmap: {created2:?}"
        );
        assert!(wt2.join("releases/v1.0/notes.txt").is_file());
        assert!(!wt2.join("heavy_unwanted").exists());
    }

    #[cfg(unix)]
    #[test]
    fn add_worktree_materializes_clean_head_preserves_modes_and_isolates_dirty_donor_edits() {
        use std::os::unix::fs::PermissionsExt;

        let sandbox = tempfile::TempDir::new().unwrap();
        let main = sandbox.path().join("main");
        std::fs::create_dir_all(main.join("src/nested")).unwrap();
        git_in(&main, &["init", "-b", "main"]);
        git_in(&main, &["config", "user.name", "Test"]);
        git_in(&main, &["config", "user.email", "test@example.com"]);
        crate::test_support::trust_repo(&main);

        std::fs::write(main.join("src/clean.rs"), "pub fn clean() -> u32 { 42 }\n").unwrap();
        std::fs::write(
            main.join("src/nested/dirty.rs"),
            "pub fn committed() -> &'static str { \"head\" }\n",
        )
        .unwrap();
        let script_path = main.join("run.sh");
        std::fs::write(&script_path, "#!/bin/sh\necho ok\n").unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::os::unix::fs::symlink("src/clean.rs", main.join("clean-link.rs")).unwrap();

        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-m", "initial"]);

        // Dirty `src/nested/dirty.rs` and add an untracked file in `main`; neither may leak!
        std::fs::write(
            main.join("src/nested/dirty.rs"),
            "pub fn uncommitted_leak() -> &'static str { \"LEAKED\" }\n",
        )
        .unwrap();
        std::fs::write(main.join("untracked.secret"), "do-not-copy\n").unwrap();

        let wt = sandbox.path().join("lane-cow");
        let created = add_worktree(
            main.to_str().unwrap(),
            wt.to_str().unwrap(),
            Some("feat/cow-checkout"),
            None,
            false,
        )
        .expect("add_worktree");
        assert_eq!(created, wt.to_str().unwrap());

        let status = crate::engine::git_cli::git_captured(&wt, &["status", "--porcelain"])
            .expect("git status in wt");
        assert_eq!(
            String::from_utf8_lossy(&status.stdout).trim(),
            "",
            "worktree must have empty git status"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("src/clean.rs")).unwrap(),
            "pub fn clean() -> u32 { 42 }\n"
        );
        assert_eq!(
            std::fs::read_to_string(wt.join("src/nested/dirty.rs")).unwrap(),
            "pub fn committed() -> &'static str { \"head\" }\n",
            "dirty donor working-tree file must NOT leak into new worktree"
        );
        assert!(!wt.join("untracked.secret").exists());
        assert!(wt
            .join("clean-link.rs")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink());
        let mode = std::fs::metadata(wt.join("run.sh"))
            .unwrap()
            .permissions()
            .mode();
        assert_ne!(
            mode & 0o111,
            0,
            "executable bit must be preserved: {mode:o}"
        );
    }

    #[test]
    fn add_worktree_with_stacked_devmap_provisions_sealed_base_lease() {
        let sandbox = tempfile::TempDir::new().unwrap();
        let anchor_fixture = crate::codeintel::tests::repo_with_one_generation();
        let repo_dir = anchor_fixture.path().to_path_buf();
        crate::test_support::trust_repo(&repo_dir);
        // A stacked base must come from a committed checkout (BaseKey::new
        // refuses a non-commit head_sha), so the fixture gets a real commit.
        crate::codeintel::tests::commit_and_stamp_head(&repo_dir);

        let wt = sandbox.path().join("wt-stacked");
        let created = add_worktree_with_devmap(
            repo_dir.to_str().unwrap(),
            wt.to_str().unwrap(),
            Some("feat/stacked"),
            None,
            false,
            None,
            false,
            DevmapMode::Stacked,
        )
        .expect("add_worktree_with_devmap Stacked");

        assert_eq!(
            created.devmap,
            DevmapOutcome::Provisioned { zero_copy: true }
        );
        let lease_path = devmap_query::paths::state_dir(&wt).join("codeintel/stack_lease.json");
        assert!(
            lease_path.is_file(),
            "expected stack_lease.json at {lease_path:?}"
        );
    }

    #[test]
    fn add_worktree_propagates_claude_account_marker_with_clean_git_status_and_info_exclude() {
        let sandbox = tempfile::TempDir::new().unwrap();
        let main = sandbox.path().join("main");
        std::fs::create_dir_all(main.join("src")).unwrap();
        git_in(&main, &["init", "-b", "main"]);
        git_in(&main, &["config", "user.name", "Test"]);
        git_in(&main, &["config", "user.email", "test@example.com"]);
        crate::test_support::trust_repo(&main);

        std::fs::write(main.join("src/lib.rs"), "pub fn ok() -> bool { true }\n").unwrap();
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-m", "init"]);

        // Pin seat in main checkout without a tracked .gitignore.
        std::fs::write(main.join(".claude-account"), "chandra\n").unwrap();

        let wt1 = sandbox.path().join("sibling-wt-one");
        add_worktree(
            main.to_str().unwrap(),
            wt1.to_str().unwrap(),
            Some("feat/seat-one"),
            None,
            false,
        )
        .expect("add_worktree wt1");

        // Second worktree branched from wt1 (linked-to-linked) also tests info/exclude idempotency.
        let wt2 = sandbox.path().join("sibling-wt-two");
        add_worktree(
            wt1.to_str().unwrap(),
            wt2.to_str().unwrap(),
            Some("feat/seat-two"),
            None,
            false,
        )
        .expect("add_worktree wt2");

        // Sparse worktree path must also inherit the seat cleanly.
        let wt_sparse = sandbox.path().join("sibling-wt-sparse");
        add_sparse_worktree_zero_copy_optional_branch(
            main.to_str().unwrap(),
            wt_sparse.to_str().unwrap(),
            Some("feat/seat-sparse"),
            None,
            false,
            &["src".to_string()],
            false,
            false,
            &mut |_| Ok(()),
        )
        .expect("add_sparse_worktree");

        for checkout in [&main, &wt1, &wt2, &wt_sparse] {
            assert_eq!(
                std::fs::read_to_string(checkout.join(".claude-account")).unwrap(),
                "chandra\n",
                "expected .claude-account in {}",
                checkout.display()
            );
            assert!(
                !checkout.join(".gitignore").exists(),
                "must never create or touch .gitignore in {}",
                checkout.display()
            );
            let status = crate::engine::git_cli::git_captured(checkout, &["status", "--porcelain"])
                .expect("git status");
            assert_eq!(
                String::from_utf8_lossy(&status.stdout).trim(),
                "",
                "git status --porcelain must stay 100% clean in {}",
                checkout.display()
            );
        }

        let common = crate::engine::git_cli::resolve_git_common_dir(&main).unwrap();
        let exclude = std::fs::read_to_string(common.join("info/exclude")).unwrap();
        let count = exclude
            .lines()
            .filter(|line| line.trim() == "/.claude-account")
            .count();
        assert_eq!(
            count, 1,
            "info/exclude must list /.claude-account once: {exclude}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn add_worktree_refuses_to_propagate_symlink_oversized_or_malformed_claude_account() {
        let sandbox = tempfile::TempDir::new().unwrap();
        let main = sandbox.path().join("main");
        std::fs::create_dir_all(&main).unwrap();
        git_in(&main, &["init", "-b", "main"]);
        git_in(&main, &["config", "user.name", "Test"]);
        git_in(&main, &["config", "user.email", "test@example.com"]);
        crate::test_support::trust_repo(&main);

        std::fs::write(main.join("README.md"), "# test\n").unwrap();
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-m", "init"]);

        let secret = sandbox.path().join("outside-secret");
        std::fs::write(&secret, "chandra\n").unwrap();

        // 1. Symlink .claude-account -> refused
        std::os::unix::fs::symlink(&secret, main.join(".claude-account")).unwrap();
        let wt_sym = sandbox.path().join("wt-sym");
        add_worktree(
            main.to_str().unwrap(),
            wt_sym.to_str().unwrap(),
            Some("feat/sym"),
            None,
            false,
        )
        .unwrap();
        assert!(wt_sym.join(".claude-account").symlink_metadata().is_err());
        std::fs::remove_file(main.join(".claude-account")).unwrap();

        // 2. Oversized (> 256 bytes) .claude-account -> refused
        std::fs::write(main.join(".claude-account"), "a".repeat(300)).unwrap();
        let wt_big = sandbox.path().join("wt-big");
        add_worktree(
            main.to_str().unwrap(),
            wt_big.to_str().unwrap(),
            Some("feat/big"),
            None,
            false,
        )
        .unwrap();
        assert!(!wt_big.join(".claude-account").exists());

        // 3. Malformed token (path traversal / multi-token) -> refused
        for bad in ["../evil\n", "two words\n", "\n"] {
            std::fs::write(main.join(".claude-account"), bad).unwrap();
            let slug = format!("wt-bad-{}", bad.len());
            let wt_bad = sandbox.path().join(&slug);
            add_worktree(
                main.to_str().unwrap(),
                wt_bad.to_str().unwrap(),
                Some(&format!("feat/{slug}")),
                None,
                false,
            )
            .unwrap();
            assert!(!wt_bad.join(".claude-account").exists(), "bad={bad:?}");
        }
    }

    #[test]
    fn add_worktree_propagates_rotated_main_seat_and_excludes_at_older_start_point() {
        let sandbox = tempfile::TempDir::new().unwrap();
        let main = sandbox.path().join("main");
        std::fs::create_dir_all(main.join("src")).unwrap();
        git_in(&main, &["init", "-b", "main"]);
        git_in(&main, &["config", "user.name", "Test"]);
        git_in(&main, &["config", "user.email", "test@example.com"]);
        crate::test_support::trust_repo(&main);

        std::fs::write(main.join("src/lib.rs"), "pub fn v1() -> u32 { 1 }\n").unwrap();
        git_in(&main, &["add", "."]);
        git_in(&main, &["commit", "-m", "v1 without gitignore"]);
        let v1_sha = String::from_utf8(
            crate::engine::git_cli::git_captured(&main, &["rev-parse", "HEAD"])
                .unwrap()
                .stdout,
        )
        .unwrap()
        .trim()
        .to_string();

        // Later commit on main adds a tracked .gitignore ignoring .claude-account.
        std::fs::write(main.join(".gitignore"), ".claude-account\n").unwrap();
        git_in(&main, &["add", ".gitignore"]);
        git_in(&main, &["commit", "-m", "v2 with gitignore"]);

        std::fs::write(main.join(".claude-account"), "chandra\n").unwrap();

        // Create wt_old at v1_sha (where .gitignore does NOT exist).
        // Because wt_old lacks .gitignore, info/exclude MUST be populated so wt_old stays clean!
        let wt_old = sandbox.path().join("wt-old");
        add_worktree(
            main.to_str().unwrap(),
            wt_old.to_str().unwrap(),
            Some("feat/from-v1"),
            Some(&v1_sha),
            false,
        )
        .expect("add_worktree at v1_sha");

        assert_eq!(
            std::fs::read_to_string(wt_old.join(".claude-account")).unwrap(),
            "chandra\n"
        );
        let status_old = crate::engine::git_cli::git_captured(&wt_old, &["status", "--porcelain"])
            .expect("git status in wt_old");
        assert_eq!(
            String::from_utf8_lossy(&status_old.stdout).trim(),
            "",
            "wt_old at older start_point without .gitignore must still have clean git status"
        );

        // Rotate seat in main checkout from chandra -> work, then create wt_from_linked using wt_old as repo_path.
        std::fs::write(main.join(".claude-account"), "work\n").unwrap();
        let wt_from_linked = sandbox.path().join("wt-from-linked");
        add_worktree(
            wt_old.to_str().unwrap(),
            wt_from_linked.to_str().unwrap(),
            Some("feat/rotated-seat"),
            None,
            false,
        )
        .expect("add_worktree from linked worktree after main seat rotation");
        assert_eq!(
            std::fs::read_to_string(wt_from_linked.join(".claude-account")).unwrap(),
            "work\n",
            "new worktree created from linked worktree must inherit rotated canonical seat from main checkout"
        );
    }
}

/// ft-b7a499bf: a worktree is read once per pass, and a shortstat is skipped
/// only when a stamp proves nothing it depends on moved.
#[cfg(test)]
mod scan_once_tests {
    use super::*;
    use crate::engine::git_cli::spawn_log::spawns_in;
    use crate::engine::ref_cache::ManualClock;
    use crate::test_support::{git_in, repo_with_worktrees};

    fn plain(path: &Path) -> PathBuf {
        crate::engine::git_cli::canonicalize_plain(path).unwrap()
    }

    fn spawned(cwds: &[PathBuf], needle: &str) -> usize {
        cwds.iter()
            .flat_map(|cwd| spawns_in(cwd))
            .filter(|argv| argv.iter().any(|arg| arg == needle))
            .count()
    }

    fn everywhere(root: &Path, linked: &[PathBuf]) -> Vec<PathBuf> {
        let mut cwds = linked.to_vec();
        cwds.push(root.to_path_buf());
        cwds
    }

    fn change_every_worktree(linked: &[PathBuf]) {
        for dir in linked {
            std::fs::write(dir.join("seed.txt"), "seed\nchanged\n").unwrap();
        }
    }

    fn gathering(root: &Path) -> bool {
        let key = FlightKey {
            repo: validate_repo(root.to_str().unwrap()).unwrap(),
            class: crate::engine::git_cli::current_admission(),
        };
        let lanes = flight_lanes().lock().unwrap();
        lanes.get(&key).is_some_and(|lane| lane.open.is_some())
    }

    /// The snapshot's main-checkout status, its rows and its collision scan are
    /// one read of each worktree: N worktrees, N statuses (it was N + 1, the
    /// main checkout twice).
    #[test]
    fn a_snapshot_spawns_one_status_per_worktree() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = plain(main.path());
        change_every_worktree(&linked);
        let cwds = everywhere(&root, &linked);
        let before = spawned(&cwds, "status");
        let snapshot = crate::insights::snapshot(root.to_str().unwrap());
        assert!(snapshot.collisions.ok, "{:?}", snapshot.collisions);
        assert_eq!(
            spawned(&cwds, "status") - before,
            cwds.len(),
            "one status per worktree"
        );
    }

    /// A full listing and a snapshot that overlap read each worktree once
    /// between them: the one that reads first serves the other, because its read
    /// began after both had asked (it was 2N + 1).
    #[test]
    fn a_full_listing_and_a_snapshot_in_flight_together_share_each_status() {
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = plain(main.path());
        change_every_worktree(&linked);
        let cwds = everywhere(&root, &linked);
        let before = spawned(&cwds, "status");
        let path = root.to_str().unwrap().to_string();
        let listing = std::thread::spawn(move || {
            GATHER_OVERRIDE.with(|cell| cell.set(Some(Duration::from_millis(400))));
            list_worktrees(&path)
        });
        let started = Instant::now();
        while !gathering(&root) {
            assert!(
                started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
                "never gathered"
            );
            std::thread::yield_now();
        }
        let snapshot = crate::insights::snapshot(root.to_str().unwrap());
        let listed = listing.join().unwrap().expect("listing");
        assert!(snapshot.collisions.ok, "{:?}", snapshot.collisions);
        assert!(
            listed[1..].iter().all(|w| w.dirty_files == Some(1)),
            "{listed:?}"
        );
        assert_eq!(
            spawned(&cwds, "status") - before,
            cwds.len(),
            "one status per worktree between the two"
        );
    }

    /// Two full listings further apart than the racy window: the second runs no
    /// `diff --shortstat`, the stamp proving nothing moved. The clock is the
    /// one `ref_cache` injects, so no test sleeps three seconds.
    #[test]
    fn an_unchanged_worktree_is_not_diffed_again_once_it_is_older_than_the_racy_window() {
        // The stamp reads HOME and XDG_CONFIG_HOME: no test may rebind them
        // under this one.
        let _serial = crate::test_support::env::env_serial();
        let clock = ManualClock::install();
        let (main, _parent, linked) = repo_with_worktrees(3);
        let root = plain(main.path());
        change_every_worktree(&linked);
        let first = list_worktrees(root.to_str().unwrap()).expect("first");
        assert_eq!(spawned(&linked, "--shortstat"), linked.len());
        clock.advance(crate::engine::ref_cache::racy_window() + Duration::from_secs(1));
        let second = list_worktrees(root.to_str().unwrap()).expect("second");
        assert_eq!(
            spawned(&linked, "--shortstat"),
            linked.len(),
            "the second listing diffed again"
        );
        assert_eq!(
            serde_json::to_value(&first).unwrap(),
            serde_json::to_value(&second).unwrap()
        );
    }

    /// Whatever changes a diff, the next listing says what a fresh measurement
    /// says, and the stamp skips the measurement only on the steps that
    /// changed nothing.
    #[test]
    fn the_listing_always_agrees_with_a_fresh_measurement_across_every_kind_of_change() {
        let _serial = crate::test_support::env::env_serial();
        let clock = ManualClock::install();
        let (main, _parent, linked) = repo_with_worktrees(1);
        let root = plain(main.path());
        let dir = linked[0].clone();
        let seed = dir.join("seed.txt");
        std::fs::write(&seed, "seed\nx\n").unwrap();
        type Step = (&'static str, Box<dyn Fn()>);
        let steps: Vec<Step> = vec![
            ("first measurement", Box::new(|| {})),
            ("untouched", Box::new(|| {})),
            ("same size, mtime restored", {
                let seed = seed.clone();
                Box::new(move || {
                    let mtime = std::fs::metadata(&seed).unwrap().modified().unwrap();
                    std::fs::write(&seed, "seed\ny\n").unwrap();
                    std::fs::File::options()
                        .write(true)
                        .open(&seed)
                        .unwrap()
                        .set_modified(mtime)
                        .unwrap();
                })
            }),
            ("another size", {
                let seed = seed.clone();
                Box::new(move || std::fs::write(&seed, "seed\nyy\nzz\n").unwrap())
            }),
            ("an untracked file", {
                let dir = dir.clone();
                Box::new(move || std::fs::write(dir.join("loose.txt"), "x").unwrap())
            }),
            ("an index-only add", {
                let dir = dir.clone();
                Box::new(move || git_in(&dir, &["add", "seed.txt"]))
            }),
            ("a commit", {
                let dir = dir.clone();
                Box::new(move || git_in(&dir, &["commit", "-q", "-m", "next"]))
            }),
            ("an edit after it", {
                let seed = seed.clone();
                Box::new(move || std::fs::write(&seed, "seed\nyy\nzz\nmore\n").unwrap())
            }),
            ("the branch moved under HEAD", {
                let dir = dir.clone();
                Box::new(move || git_in(&dir, &["update-ref", "refs/heads/agent/00", "HEAD~1"]))
            }),
            ("a rename", {
                let dir = dir.clone();
                Box::new(move || {
                    git_in(&dir, &["mv", "seed.txt", "moved.txt"]);
                    std::fs::write(dir.join("moved.txt"), "seed\nmoved\n").unwrap();
                })
            }),
            ("settled again", Box::new(|| {})),
        ];
        let mut skipped = 0;
        let mut trace = Vec::new();
        for (label, step) in steps {
            step();
            let before = spawned(std::slice::from_ref(&dir), "--shortstat");
            let listed = list_worktrees(root.to_str().unwrap()).expect("list");
            let measured_by_listing = spawned(std::slice::from_ref(&dir), "--shortstat") - before;
            let wt = listed
                .iter()
                .find(|w| Path::new(&w.path) == dir)
                .expect("listed");
            assert_eq!(wt.scan_note, None, "{label}");
            assert_eq!(wt.diff_stat, measure_diff_stat(&dir), "{label}");
            trace.push((label, measured_by_listing));
            // Time passes between listings: the injected clock stands still
            // otherwise, and a later pass would read as older than an entry
            // an earlier one stored.
            clock.advance(Duration::from_secs(1));
            if measured_by_listing == 0 && wt.dirty_files != Some(0) {
                skipped += 1;
            }
        }
        assert!(
            skipped >= 1,
            "the stamp never skipped: the test proves nothing ({trace:?})"
        );
    }

    /// A stamp that cannot be proven never skips and is never kept.
    #[test]
    fn an_unprovable_stamp_measures_every_time() {
        let _serial = crate::test_support::env::env_serial();
        let (main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        std::fs::write(dir.join("seed.txt"), "seed\nchanged\n").unwrap();
        std::fs::create_dir(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub").join("f.txt"), "f").unwrap();
        let status: Arc<str> = Arc::from(" M seed.txt\0");
        let stat = Some(WorktreeDiffStat::default());
        let measures = |status: &Arc<str>, dir: &Path, times: usize| {
            let pass = ScanPass::now();
            let mut ran = 0;
            for _ in 0..times {
                diff_stamp::through(dir, status, &pass, || {
                    ran += 1;
                    stat.clone()
                });
            }
            ran
        };

        // Control: with settled timestamps and nothing in the way, the second
        // ask is served from the stamp.
        {
            let clock = ManualClock::install();
            let _ = clock.now();
            assert_eq!(measures(&status, &dir, 2), 1, "control");
        }
        // Racy: a file written inside the window cannot be told from a later
        // write in the same tick.
        {
            let clock = ManualClock::install();
            let soon = clock.now() + Duration::from_secs(10);
            std::fs::File::options()
                .write(true)
                .open(dir.join("seed.txt"))
                .unwrap()
                .set_modified(soon)
                .unwrap();
            assert_eq!(measures(&status, &dir, 2), 2, "racy");
        }
        let _clock = ManualClock::install();
        std::fs::File::options()
            .write(true)
            .open(dir.join("seed.txt"))
            .unwrap()
            .set_modified(std::time::SystemTime::now() - Duration::from_secs(600))
            .unwrap();
        // A changed path that cannot be read.
        let unreadable: Arc<str> = Arc::from(" M sub/f.txt\0");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let sub = dir.join("sub");
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o000)).unwrap();
            let ran = measures(&unreadable, &dir, 2);
            std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(ran, 2, "an unreadable path");
        }
        // A changed path that is a directory (a submodule's own commits move
        // nothing in this worktree).
        let nested: Arc<str> = Arc::from(" M sub\0");
        assert_eq!(measures(&nested, &dir, 2), 2, "a directory row");
        // An untracked attributes file changes how tracked files diff.
        let attributes: Arc<str> = Arc::from("?? .gitattributes\0 M seed.txt\0");
        assert_eq!(measures(&attributes, &dir, 2), 2, "untracked attributes");
        // A worktree that is gone.
        let gone = main.path().join("no-such-worktree");
        assert_eq!(measures(&status, &gone, 2), 2, "a missing directory");
        // A deletion explains an absent path; nothing else does.
        std::fs::remove_file(dir.join("sub").join("f.txt")).unwrap();
        let deleted: Arc<str> = Arc::from(" D sub/f.txt\0");
        assert_eq!(measures(&deleted, &dir, 2), 1, "a deleted path is provable");
        let vanished: Arc<str> = Arc::from(" M sub/f.txt\0");
        assert_eq!(
            measures(&vanished, &dir, 2),
            2,
            "an absent path with no deletion"
        );
    }

    /// Two states that differ only in HEAD's commit, or only in the ctime of a
    /// changed file, have different stamps.
    #[test]
    fn a_stamp_sees_a_moved_head_and_an_edit_that_kept_size_and_mtime() {
        let _serial = crate::test_support::env::env_serial();
        let _clock = ManualClock::install();
        let (_main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        let seed = dir.join("seed.txt");
        std::fs::write(&seed, "seed\nchanged\n").unwrap();
        let status: Arc<str> = Arc::from(" M seed.txt\0");
        let capture = || diff_stamp::DiffStamp::capture(&dir, &status, &ScanPass::now());

        let base = capture().expect("a provable stamp");
        assert_eq!(capture().as_ref(), Some(&base), "stable while untouched");

        let mtime = std::fs::metadata(&seed).unwrap().modified().unwrap();
        std::fs::write(&seed, "seed\nCHANGED\n").unwrap();
        std::fs::File::options()
            .write(true)
            .open(&seed)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        let edited = capture().expect("a provable stamp");
        assert_ne!(edited, base, "same size and mtime, new ctime");

        // A new commit on the checked-out branch by plumbing: neither the
        // index nor a file moves, only the branch under HEAD.
        let next = git_text(
            &dir,
            &[
                "commit-tree",
                "HEAD^{tree}",
                "-p",
                "HEAD",
                "-m",
                "moves HEAD only",
            ],
        )
        .unwrap();
        git_in(&dir, &["update-ref", "refs/heads/agent/00", next.trim()]);
        let moved = capture().expect("a provable stamp");
        assert_ne!(
            moved, edited,
            "same status bytes and files, new HEAD commit"
        );
    }

    /// One status per worktree whatever the number of asks inside one pass, and
    /// a read begun before a caller asked is never its answer.
    #[test]
    fn a_status_read_is_reused_only_if_it_began_after_the_ask() {
        let (_main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        let early = Instant::now();
        let first = read_status_since(&dir, None, early).unwrap();
        assert!(first.led);
        let again = read_status_since(&dir, None, early).unwrap();
        assert!(!again.led, "a read since the ask is shared");
        assert_eq!(again.started, first.started);
        let later = read_status_since(&dir, None, Instant::now()).unwrap();
        assert!(
            later.led,
            "a read that began before the ask is not its answer"
        );
        assert_eq!(spawned(std::slice::from_ref(&dir), "status"), 2);
    }

    /// A failure is not kept: once the worktree is back the next ask reads it.
    #[test]
    fn a_failed_read_is_never_kept() {
        let (main, _parent, _linked) = repo_with_worktrees(0);
        let root = plain(main.path());
        let early = Instant::now();
        let probe = |_: &Path| {};
        let ok = status_probes::with_probe(std::slice::from_ref(&root), probe, || {
            read_status_since(&root, None, early)
        });
        assert!(ok.is_ok());
        let expired = ScanDeadline::after(Instant::now(), Duration::ZERO);
        let cut = read_status_since(&root, Some(&expired), Instant::now());
        assert!(cut.is_err());
        assert!(
            read_status_since(&root, None, Instant::now()).is_ok(),
            "the cut left no failure behind"
        );
    }

    /// A deadline-less read that ends at git's own time limit ends every caller
    /// waiting on it, together: six waiters on a hung worktree used to retry one
    /// after another and finish at six times the limit.
    #[test]
    fn callers_waiting_on_a_hung_read_fail_with_it_together() {
        let (_main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        let early = Instant::now();
        let started = Instant::now();
        let hang = Duration::from_millis(300);
        let results = status_probes::with_probe(
            std::slice::from_ref(&dir),
            move |_| std::thread::sleep(hang),
            || {
                status_probes::with_forced_timeout(&dir, || {
                    std::thread::scope(|scope| {
                        let waiters: Vec<_> = (0..6)
                            .map(|_| scope.spawn(|| read_status_since(&dir, None, early)))
                            .collect();
                        waiters
                            .into_iter()
                            .map(|waiter| waiter.join().unwrap())
                            .collect::<Vec<_>>()
                    })
                })
            },
        );
        let took = started.elapsed();
        for result in &results {
            let error = result.as_ref().expect_err("the read hung");
            assert!(error.message().contains("timed out"), "{error:?}");
        }
        assert!(
            took < crate::test_support::coverage_relaxed(hang * 3),
            "six waiters took {took:?} for a {hang:?} hang: they retried one at a time"
        );
    }
}

#[cfg(test)]
mod worktree_list_porcelain_tests {
    use super::worktree_list_porcelain_argv;

    #[test]
    fn porcelain_list_argv_is_the_line_the_gate_judges() {
        assert_eq!(
            worktree_list_porcelain_argv(),
            vec!["git", "worktree", "list", "--porcelain", "-z"]
        );
    }
}

/// Adversarial attacks on ft-b7a499bf (adversary, wave 2).
#[cfg(test)]
mod scan_once_adversarial_tests {
    use super::*;
    use crate::engine::ref_cache::ManualClock;
    use crate::test_support::env::{bind_env, env_serial};
    use crate::test_support::{git_in, repo_with_worktrees};
    use std::sync::Arc;

    /// Runs the shortstat through the stamp twice, `change` between them, and
    /// says how many times the measurement ran and whether the second answer
    /// differs from a fresh measurement.
    fn measured_twice(dir: &Path, clock: &ManualClock, change: impl FnOnce()) -> (usize, bool) {
        let status: Arc<str> = Arc::from(" M seed.txt\0");
        let runs = std::cell::Cell::new(0usize);
        let measure = || {
            runs.set(runs.get() + 1);
            measure_diff_stat(dir)
        };
        let first = diff_stamp::through(dir, &status, &ScanPass::now(), measure);
        assert!(first.is_some());
        change();
        clock.advance(crate::engine::ref_cache::racy_window() + Duration::from_secs(1));
        let second = diff_stamp::through(dir, &status, &ScanPass::now(), measure);
        (runs.get(), second != measure_diff_stat(dir))
    }

    /// A user-level attributes file (`$XDG_CONFIG_HOME/git/attributes`, which
    /// git reads for every diff) is an input of `git diff HEAD --shortstat`
    /// that the stamp does not carry: `-diff` turns the line counts into a
    /// binary summary.
    #[test]
    fn a_user_attributes_file_edit_is_not_served_a_stale_shortstat() {
        let serial = env_serial();
        let clock = ManualClock::install();
        let (_main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        std::fs::write(dir.join("seed.txt"), "seed\nchanged\n").unwrap();
        let home = tempfile::tempdir().unwrap();
        let _env = bind_env(&serial)
            .set("HOME", home.path())
            .set("XDG_CONFIG_HOME", home.path());
        let attributes = home.path().join("git").join("attributes");
        let (runs, stale) = measured_twice(&dir, &clock, || {
            std::fs::create_dir_all(attributes.parent().unwrap()).unwrap();
            std::fs::write(&attributes, "seed.txt -diff\n").unwrap();
        });
        assert_eq!(runs, 2, "the attributes file changed, the stamp did not");
        assert!(!stale, "a stale shortstat was served");
    }

    /// Same, through `core.attributesFile`: the config that names the file is
    /// stamped, the file it names is not.
    #[test]
    fn an_edit_to_the_file_core_attributes_file_names_is_not_served_stale() {
        let _serial = env_serial();
        let clock = ManualClock::install();
        let (_main, _parent, linked) = repo_with_worktrees(1);
        let dir = linked[0].clone();
        std::fs::write(dir.join("seed.txt"), "seed\nchanged\n").unwrap();
        let outside = tempfile::tempdir().unwrap();
        let attributes = outside.path().join("attrs");
        std::fs::write(&attributes, "# nothing yet\n").unwrap();
        git_in(
            &dir,
            &[
                "config",
                "core.attributesFile",
                attributes.to_str().unwrap(),
            ],
        );
        let (runs, stale) = measured_twice(&dir, &clock, || {
            std::fs::write(&attributes, "seed.txt -diff\n").unwrap();
        });
        assert_eq!(runs, 2, "the named file changed, the stamp did not");
        assert!(!stale, "a stale shortstat was served");
    }
}
