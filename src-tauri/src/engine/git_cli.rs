use crate::diff::{Expect, WriteFailure};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
#[cfg(test)]
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
#[cfg(any(not(unix), test))]
use std::thread;
use std::time::{Duration, Instant};

pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(90);
/// Ceiling for network-bound operations (`clone`, `fetch`, `push`, remote
/// `ls-remote`) where multi-gigabyte transfers are legitimate. Local plumbing
/// keeps [`DEFAULT_TIMEOUT`].
pub const NETWORK_TIMEOUT: Duration = Duration::from_secs(30 * 60);
pub const MAX_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

#[cfg(unix)]
mod pipe_drain;

#[cfg(all(test, unix))]
mod grace_adversarial;

#[cfg(any(not(unix), test))]
mod thread_io;

mod shared_budget;

#[cfg(test)]
mod awaiting_adversarial_tests;

#[cfg(test)]
mod budget_churn_tests;

#[cfg(test)]
mod run_errors_adversarial_tests;

pub(crate) mod hooks;
pub use hooks::{
    cancel as cancel_hooked_git, is_cancelled as is_hooked_git_cancelled, HOOK_TIMEOUT,
};

mod preflight;
pub use preflight::{
    git_preflight, require_git_for_clone, GitPreflight, GitPreflightStatus, MIN_CLONE_GIT_VERSION,
    MIN_GIT_VERSION, MIN_SUBMODULE_GIT_VERSION,
};
pub(crate) use preflight::{
    git_supports, parse_git_version, supports, COMMIT_GRAPH_CHANGED_PATHS, DIFF_MERGES_OPTION,
    FORCE_IF_INCLUDES, SINCE_AS_FILTER,
};

/// Shared grace window for pipe EOF after child exit. Unix closes unfinished
/// descriptors; Windows cancels workers and retains their resource slots until
/// they exit. This cleanup grace is separate from the command deadline.
const DRAIN_JOIN_GRACE: Duration = Duration::from_secs(2);

/// Canonical work tree or bare repository resolved from a user-supplied path.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedRepo {
    pub path: String,
    pub name: String,
    pub is_bare: bool,
    /// Canonical common Git directory: one value shared by every checkout of
    /// a repository, which is how the tab strip knows two tabs are worktrees
    /// of the same thing. `None` when the metadata could not be read — a
    /// tab with an unknown family stands alone rather than joining a guess.
    pub common_dir: Option<String>,
}

/// Admits an explicitly trusted Git work tree or bare repository.
pub fn validate_repo(repo_path: &str) -> Result<PathBuf, CommandError> {
    let canonical = validate_repo_path(repo_path)?;
    crate::repository_trust::require(&canonical)?;
    Ok(canonical)
}

/// Filesystem-only discovery for trust preview and cleanup. This grants no
/// execution authority; ordinary readers and writers use [`validate_repo`].
///
/// A work tree is accepted when `.git` exists as a directory or a gitfile (linked worktrees).
/// A bare repo is accepted when both `HEAD` and `objects` are present.
/// Discovery never invokes Git.
/// Always returns the canonical path.
pub fn validate_repo_path(repo_path: &str) -> Result<PathBuf, String> {
    if repo_path.is_empty() || repo_path.contains('\0') || repo_path.chars().any(|c| c.is_control())
    {
        return Err("Invalid repository path".into());
    }
    let path = Path::new(repo_path);
    if !path.is_absolute() {
        return Err("Repository path must be absolute".into());
    }
    let canonical = path
        .canonicalize()
        .map_err(|e| format!("Cannot access path '{}': {}", repo_path, e))?;
    if !canonical.is_dir() {
        return Err(format!("Not a directory: {}", canonical.display()));
    }
    if is_git_repository(&canonical) {
        return Ok(canonical);
    }
    Err(format!("Not a Git repository: {}", canonical.display()))
}

pub(crate) fn is_git_repository(canonical: &Path) -> bool {
    if canonical.join(".git").exists() {
        return true;
    }
    has_bare_layout(canonical)
}

fn has_bare_layout(path: &Path) -> bool {
    path.join("HEAD").is_file() && path.join("objects").is_dir()
}

fn rev_parse_is_bare(path: &Path) -> bool {
    match git_text(path, &["rev-parse", "--is-bare-repository"]) {
        Ok(text) => text.trim().eq_ignore_ascii_case("true"),
        Err(_) => false,
    }
}

/// Resolves the private Git directory from bounded filesystem metadata.
pub fn resolve_git_dir(repo: &Path) -> Result<PathBuf, CommandError> {
    crate::repository_trust::git_directories(repo).map(|(private, _)| private)
}

/// Resolves the common Git directory without executing sibling configuration.
pub fn resolve_git_common_dir(repo: &Path) -> Result<PathBuf, CommandError> {
    crate::repository_trust::git_directories(repo).map(|(_, common)| common)
}

/// Canonicalizes `repo_path` and reports whether it is a bare repository.
pub fn resolve_repo(repo_path: &str) -> Result<ResolvedRepo, CommandError> {
    let canonical = validate_repo(repo_path)?;
    let is_bare = rev_parse_is_bare(&canonical)
        || (!canonical.join(".git").exists() && has_bare_layout(&canonical));
    let name = canonical
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "repo".to_string());
    let common_dir = resolve_git_common_dir(&canonical)
        .ok()
        .map(|dir| dir.to_string_lossy().into_owned());
    Ok(ResolvedRepo {
        path: canonical.to_string_lossy().into_owned(),
        name,
        is_bare,
        common_dir,
    })
}

/// Walks from `path` (file or directory) up to the nearest work tree that contains `.git`.
///
/// Used for Finder "Open With", Dock drops, and in-window folder drops, which often
/// hand us a nested file rather than the repository root.
pub fn find_git_root(path: &Path) -> Option<PathBuf> {
    if path.as_os_str().is_empty() {
        return None;
    }
    let start = if path.is_file() {
        path.parent()?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    let mut current = start.canonicalize().ok()?;
    loop {
        if current.join(".git").exists() || has_bare_layout(&current) {
            return Some(current);
        }
        if !current.pop() {
            return None;
        }
    }
}

/// Resolves a relative path inside a repository, rejecting absolute paths and `..` escapes.
pub fn sandbox_join(repo: &Path, file_path: &str) -> Result<PathBuf, String> {
    if file_path.is_empty() || file_path.contains('\0') {
        return Err("Invalid file path".into());
    }
    let rel = Path::new(file_path);
    if rel.is_absolute() {
        return Err("File path must be relative to the repository".into());
    }
    for component in rel.components() {
        match component {
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err("File path escapes the repository".into());
            }
            Component::Normal(name) if is_git_metadata_component(name) => {
                return Err(git_metadata_message(file_path));
            }
            Component::CurDir | Component::Normal(_) => {}
        }
    }
    let joined = repo.join(rel);
    Ok(joined)
}

fn git_metadata_message(file_path: &str) -> String {
    format!("Git metadata cannot be read or written as a repository file: {file_path}")
}

/// True when one path component names git's own metadata directory, in any
/// spelling the platform's filesystem would resolve to it. Mirrors what git's
/// `verify_path` refuses in a checkout: `.git` compared case-insensitively
/// after dropping trailing dots and spaces (Windows ignores both), an NTFS
/// alternate-data-stream suffix (`.git::$INDEX_ALLOCATION`), and on Windows
/// the 8.3 short name `git~1`. `.gitignore`, `.gitkeep`, `.github` and
/// `.gitattributes` are ordinary files and pass.
///
/// Also refused, as git's `is_hfs_dotgit` does, is any spelling that differs
/// from `.git` only by the code points HFS+ ignores when it compares names
/// ([`HFS_IGNORABLE`]); and `git~1` on every platform, as `is_ntfs_dotgit` does
/// (git applies `core.protectNTFS` everywhere by default).
///
/// This is the lexical half. A symlink into the git directory, a hashed 8.3
/// name, or a `gitdir:` layout with another name is caught by the resolved
/// path in [`sandbox_join_write`], including a nested repository's real git
/// directory (see [`inside_nested_git_directory`]).
pub fn is_git_metadata_component(name: &std::ffi::OsStr) -> bool {
    // A backslash is a directory separator on Windows, and git applies
    // `core.protectNTFS` everywhere (`is_dir_sep`): `.git\config` is
    // `.git/config` there (CVE-2019-1354), so every piece is tested.
    named_like(name, |base| {
        base.eq_ignore_ascii_case(".git") || base.eq_ignore_ascii_case("git~1")
    })
}

/// True when any `\`-separated piece of `name`, after dropping HFS-ignorable
/// code points, an NTFS stream suffix and trailing dots and spaces, satisfies
/// `is_target` (the shared spelling rules of git's `is_hfs_dot*` and
/// `is_ntfs_dot*`).
fn named_like(name: &std::ffi::OsStr, is_target: impl Fn(&str) -> bool) -> bool {
    let name = name.to_string_lossy();
    // HFS+ drops these code points wherever they sit, so `.g\u{200c}it` is
    // `.git` there (git: `next_hfs_char` skipping `ignorable` code points).
    let name: String = name.chars().filter(|c| !is_hfs_ignorable(*c)).collect();
    name.split('\\').any(|piece| {
        // `.git:stream` and `.git::$INDEX_ALLOCATION` open the directory on
        // NTFS (git: `is_ntfs_dotgit` stops at `:`).
        let base = piece.split(':').next().unwrap_or("");
        // Windows ignores trailing dots and spaces (git: `is_ntfs_dotgit`).
        is_target(base.trim_end_matches(['.', ' ']))
    })
}

/// True when `name` is git's `.gitmodules` in any spelling `verify_path`
/// treats as it (`is_hfs_dotgitmodules`, `is_ntfs_dotgitmodules`, whose 8.3
/// short name is `gitmod~1`). Git refuses it as a symlink: a link there lets a
/// later `git submodule` read a file outside the tree.
pub fn is_gitmodules_component(name: &std::ffi::OsStr) -> bool {
    named_like(name, |base| {
        base.eq_ignore_ascii_case(".gitmodules") || base.eq_ignore_ascii_case("gitmod~1")
    })
}

/// Most directory entries [`refuse_nested_gitdir_below`] examines.
const NESTED_GITDIR_SCAN_ENTRIES: usize = 50_000;
/// Deepest directory level [`refuse_nested_gitdir_below`] descends.
const NESTED_GITDIR_SCAN_DEPTH: usize = 64;

/// For a delete or move of the directory `abs` (the entry named `path`):
/// refuses when any descendant is the real git directory of a nested
/// repository. [`inside_nested_git_directory`] looks upward and cannot see
/// this: `git clean -f -d` spares a nested repository only through the `.git`
/// it finds inside, and a separate gitdir (`sub/.git` -> `../.modules/sub`) has
/// none, so deleting its parent wipes the history and moving it orphans the
/// work tree.
///
/// A directory named `.git` is skipped, not descended: it is a work tree's own
/// metadata and travels with it. Symlinks are never followed ([`is_git_directory`]
/// answers false for one, and the walk does not enter it). The walk is bounded
/// by [`NESTED_GITDIR_SCAN_ENTRIES`] entries and [`NESTED_GITDIR_SCAN_DEPTH`]
/// levels, and hitting either REFUSES: a walk cut short is not a clean answer.
/// A file or symlink at `abs` has nothing below it.
pub fn refuse_nested_gitdir_below(abs: &Path, path: &str) -> Result<(), String> {
    refuse_nested_gitdir_below_capped(
        abs,
        path,
        NESTED_GITDIR_SCAN_ENTRIES,
        NESTED_GITDIR_SCAN_DEPTH,
    )
}

fn refuse_nested_gitdir_below_capped(
    abs: &Path,
    path: &str,
    max_entries: usize,
    max_depth: usize,
) -> Result<(), String> {
    let is_plain_dir =
        |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|meta| meta.file_type().is_dir());
    if !is_plain_dir(abs) {
        return Ok(());
    }
    let too_large = || {
        format!(
            "{path} is too large to check for nested git directories (more than \
             {max_entries} entries or {max_depth} levels deep); \
             change its contents in smaller parts"
        )
    };
    let mut seen = 0usize;
    let mut pending = vec![(abs.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = pending.pop() {
        if is_git_directory(&dir) {
            let inside = dir.strip_prefix(abs).unwrap_or(Path::new(""));
            let at = Path::new(path).join(inside);
            return Err(format!(
                "{path} contains the git directory of a nested repository at {}",
                at.display()
            ));
        }
        let entries = std::fs::read_dir(&dir).map_err(|e| {
            format!(
                "Cannot check {} for nested git directories: {e}",
                dir.display()
            )
        })?;
        for entry in entries {
            let entry = entry
                .map_err(|e| format!("Cannot check {path} for nested git directories: {e}"))?;
            seen += 1;
            if seen > max_entries {
                return Err(too_large());
            }
            let child = entry.path();
            // `file_type` does not follow a symlink.
            let is_dir = entry.file_type().is_ok_and(|kind| kind.is_dir());
            if !is_dir || entry.file_name() == ".git" {
                continue;
            }
            if depth + 1 > max_depth {
                return Err(too_large());
            }
            pending.push((child, depth + 1));
        }
    }
    Ok(())
}

/// The code points git's `next_hfs_char` (`is_hfs_dotgit`, utf8.c
/// `next_hfs_char`/`ignorable`) skips: zero-width and bidi controls, the
/// deprecated format controls, and the byte-order mark.
fn is_hfs_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200c}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{206a}'..='\u{206f}' | '\u{feff}'
    )
}

/// git's own `is_git_directory` test: a `HEAD` file, an `objects` directory and
/// a `refs` directory. Symlinks are not followed for `objects` and `refs`,
/// which git requires to be real directories in a gitdir it creates.
fn is_git_directory(dir: &Path) -> bool {
    // A symlink is the entry itself, not the directory it names: moving or
    // deleting a link whose target is a gitdir never touches the gitdir.
    if std::fs::symlink_metadata(dir).is_ok_and(|meta| meta.file_type().is_symlink()) {
        return false;
    }
    let is_dir = |name: &str| {
        std::fs::symlink_metadata(dir.join(name)).is_ok_and(|meta| meta.file_type().is_dir())
    };
    std::fs::metadata(dir.join("HEAD")).is_ok_and(|meta| meta.is_file())
        && is_dir("objects")
        && is_dir("refs")
}

/// True when `resolved` is a git directory or lies inside one, at any depth
/// below `root`: a nested repository or submodule whose `.git` FILE names its
/// real git directory elsewhere in the tree (`sub/.git` -> `gitdir:
/// ../.modules/sub`), or a bare layout under another name.
///
/// This reads what the `.git` file points at from the other end. The real
/// gitdir is not an ancestor of anything reachable from `sub/`, so parsing
/// `gitdir:` lines would need an index of every in-tree `.git` file (a tree
/// walk per write, or `git ls-files` plus a cache the index does not stamp).
/// A git directory is, however, recognisable from the target's own ancestor
/// chain: git itself decides "this is a gitdir" by `HEAD`, `objects/` and
/// `refs/` ([`is_git_directory`]), whatever it is named and whichever `.git`
/// file points at it, and a `gitdir:` that points outside the repository can
/// never contain a target already resolved inside it. Cost: at most three
/// `stat` calls per ancestor between the target and the repository root, no
/// process spawn, no cache and nothing to go stale. The repository root itself
/// is never tested, so a bare-layout checkout is not locked entirely.
fn inside_nested_git_directory(root: &Path, resolved: &Path) -> bool {
    resolved
        .ancestors()
        .take_while(|ancestor| *ancestor != root && ancestor.starts_with(root))
        .any(is_git_directory)
}

/// [`sandbox_join_canonical`] for a path about to be written, read for write
/// authority or moved: it also refuses a target that resolves into the
/// repository's private or common git directory. The lexical check in
/// [`sandbox_join`] cannot see a symlink alias of `.git` or a `.bare` layout,
/// where the git directory has another name. Both directories come from
/// `repository_trust::git_directories`, which reads the `.git` file and
/// spawns no git.
///
/// It follows a symlink at the final component, which is what a save wants
/// (the bytes go to what the name resolves to). A create, move or delete acts
/// on the entry itself and uses [`sandbox_join_write_entry`].
pub fn sandbox_join_write(repo: &Path, file_path: &str) -> Result<PathBuf, CommandError> {
    let resolved = sandbox_join_canonical(repo, file_path)?;
    guard_write_target(repo, file_path, resolved)
}

/// [`sandbox_join_write`] for an operation on the entry itself (create, move,
/// delete): the parent is resolved and checked, and the final name is appended
/// as it stands, so a symlink is the link and a dangling link is a name that
/// exists, not an error.
///
/// The git-metadata refusal still holds: the name itself is checked
/// lexically, and a parent that resolves into the git directory is refused. A
/// link whose TARGET is in `.git` can be moved or deleted as a link, which
/// never touches `.git`. The repository root is never a target, however it is
/// spelled.
pub fn sandbox_join_write_entry(repo: &Path, file_path: &str) -> Result<PathBuf, CommandError> {
    if Path::new(file_path)
        .components()
        .all(|component| matches!(component, Component::CurDir))
    {
        return Err(root_message(file_path).into());
    }
    let resolved = sandbox_join_entry(repo, file_path)?;
    guard_write_target(repo, file_path, resolved)
}

fn root_message(file_path: &str) -> String {
    format!("Refusing to change the repository root: {file_path}")
}

/// What no write target may be: the repository root, or anything at or under
/// its private or common git directory.
fn guard_write_target(
    repo: &Path,
    file_path: &str,
    resolved: PathBuf,
) -> Result<PathBuf, CommandError> {
    let root = repo
        .canonicalize()
        .map_err(|e| format!("Cannot resolve repository path '{}': {}", repo.display(), e))?;
    if resolved == root {
        return Err(root_message(file_path).into());
    }
    let (private, common) = crate::repository_trust::git_directories(repo)?;
    for dir in [private, common] {
        let dir = dir.canonicalize().unwrap_or(dir);
        if resolved.starts_with(&dir) {
            return Err(git_metadata_message(file_path).into());
        }
    }
    if inside_nested_git_directory(&root, &resolved) {
        return Err(git_metadata_message(file_path).into());
    }
    Ok(resolved)
}

/// Like [`sandbox_join`], but safe against symlinks that point outside the repo.
///
/// `sandbox_join` only validates the path lexically, so a symlink placed inside
/// the repository would silently redirect reads and writes past the repo
/// boundary. This helper performs the same lexical validation, then canonicalizes
/// the repository root (TempDir paths may live behind `/var` -> `/private/var`)
/// and walks the relative path one component at a time: every EXISTING prefix
/// is resolved through `symlink_metadata` + `canonicalize` and re-checked
/// against the canonical repo prefix, so intermediate symlinks — including
/// dangling ones whose target lies outside — are caught before any filesystem
/// use. Non-existent trailing components are appended lexically, which keeps
/// "create new nested directories" flows working unchanged.
pub fn sandbox_join_canonical(repo: &Path, file_path: &str) -> Result<PathBuf, String> {
    // Lexical validation (absolute/..//NUL rejection) is owned by sandbox_join.
    let _validated = sandbox_join(repo, file_path)?;
    let repo_canonical = repo
        .canonicalize()
        .map_err(|e| format!("Cannot resolve repository path '{}': {}", repo.display(), e))?;
    let mut current = repo_canonical.clone();
    for component in Path::new(file_path).components() {
        let name = match component {
            Component::Normal(name) => name,
            // sandbox_join already accepted only CurDir besides Normal; a `.`
            // component is a semantic no-op on a canonical base.
            Component::CurDir => continue,
            _ => continue,
        };
        current.push(name);
        match std::fs::symlink_metadata(&current) {
            Ok(_) => {
                // The component exists (file, dir, or symlink): resolve it and
                // re-verify containment. A dangling symlink lands here too —
                // the link itself exists — and fails canonicalize below.
                let resolved = current
                    .canonicalize()
                    .map_err(|e| format!("Cannot resolve '{}': {}", current.display(), e))?;
                if !resolved.starts_with(&repo_canonical) {
                    return Err(symlink_escape_message(file_path));
                }
                current = resolved;
            }
            Err(_) => {
                // Does not exist yet: remaining components stay purely lexical,
                // which is safe because `..`/absolute/NUL were already rejected.
            }
        }
    }
    Ok(current)
}

fn symlink_escape_message(file_path: &str) -> String {
    format!("File path escapes the repository via symlink: {file_path}")
}

/// True when [`sandbox_join_canonical`] refused a path because a symlink
/// resolved outside the repository. Callers that inspect the Git *entry*
/// (coverage of a vendor link, `read_link`) match this instead of treating
/// the refusal as a user-facing failure. Write paths must still fail closed.
pub fn is_sandbox_symlink_escape(err: &str) -> bool {
    err.contains("escapes the repository via symlink")
}

/// Resolve the parent of a Git entry without following the entry itself.
/// A symlink is a blob of mode 120000 in Git, including dangling links. This
/// path is only for operations that inspect that entry (Git diff or read_link),
/// never for reading or writing its target. Parent escapes remain forbidden.
pub fn sandbox_join_entry(repo: &Path, file_path: &str) -> Result<PathBuf, String> {
    sandbox_join(repo, file_path)?;
    let rel = Path::new(file_path);
    if file_path.ends_with('/') || file_path.ends_with("/.") {
        return Err("Expected a file entry, not a directory traversal".into());
    }
    let name = rel.file_name().ok_or("Expected a file entry")?;
    let parent = rel.parent().filter(|p| !p.as_os_str().is_empty());
    let parent = match parent {
        Some(parent) => {
            sandbox_join_canonical(repo, parent.to_str().ok_or("Invalid file parent")?)?
        }
        None => repo
            .canonicalize()
            .map_err(|e| format!("Cannot resolve repository: {e}"))?,
    };
    Ok(parent.join(name))
}

pub fn sandbox_write(repo_path: &str, file_path: &str, content: &str) -> Result<(), CommandError> {
    sandbox_write_expecting(repo_path, file_path, None, content).map(|_| ())
}

/// The one sandboxed write. With `expected` the file is replaced only if it
/// still holds exactly those bytes: `Ok(false)` means it does not (changed, or
/// gone) and nothing was written, `Ok(true)` that the content was written. The
/// bytes are compared under the same anchored read the replacement uses.
/// `None` writes unconditionally, which is [`sandbox_write`].
pub fn sandbox_write_expecting(
    repo_path: &str,
    file_path: &str,
    expected: Option<&str>,
    content: &str,
) -> Result<bool, CommandError> {
    let dest = write_target(repo_path, file_path)?;
    let expect = expected.map_or(Expect::Any, |bytes| Expect::Bytes(bytes.as_bytes()));
    match crate::diff::write_regular_expecting(&dest, expect, |_| Ok(content.as_bytes().to_vec())) {
        Ok(_) => Ok(true),
        Err(WriteFailure::Changed { .. }) => Ok(false),
        Err(failure) => Err(failure.to_string().into()),
    }
}

/// The sandboxed target of a write: inside the repository, through no symlink
/// that leaves it, and not git's own metadata.
fn write_target(repo_path: &str, file_path: &str) -> Result<PathBuf, CommandError> {
    let repo = validate_repo(repo_path)?;
    sandbox_join_write(&repo, file_path)
}

/// Why a file write through [`sandbox_create`] or [`sandbox_save_edited_text`]
/// did not happen. It crosses the IPC boundary as `{code, message, ...}`;
/// every variant carries a `message` fit to show, and the code is what the
/// editor branches on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "code", rename_all = "snake_case")]
pub enum FileWriteError {
    /// A create found something at the path. Nothing was written.
    AlreadyExists { message: String, path: String },
    /// The save's base is not what is on disk now. `expected` is the base the
    /// draft carried and `actual` the token of the disk, so the editor can
    /// re-save over exactly that version if the user chooses to.
    ChangedOnDisk {
        message: String,
        path: String,
        expected: String,
        actual: String,
    },
    /// Refused or failed before anything was written.
    Refused { message: String },
}

impl FileWriteError {
    pub fn message(&self) -> &str {
        match self {
            Self::AlreadyExists { message, .. }
            | Self::ChangedOnDisk { message, .. }
            | Self::Refused { message } => message,
        }
    }

    fn from_failure(failure: WriteFailure, path: &str, expected: Option<&str>) -> Self {
        match failure {
            WriteFailure::AlreadyExists => Self::AlreadyExists {
                message: format!("'{path}' already exists"),
                path: path.to_string(),
            },
            WriteFailure::Changed { actual } => Self::ChangedOnDisk {
                message: format!(
                    "'{path}' changed on disk since it was opened. Nothing was written."
                ),
                path: path.to_string(),
                expected: expected.unwrap_or_default().to_string(),
                actual,
            },
            WriteFailure::Other(message) => Self::Refused { message },
        }
    }
}

impl std::fmt::Display for FileWriteError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

impl std::error::Error for FileWriteError {}

impl From<String> for FileWriteError {
    fn from(message: String) -> Self {
        Self::Refused { message }
    }
}

/// A path the save could not resolve: this error is its own wire type, so the
/// text is what it carries.
impl From<CommandError> for FileWriteError {
    fn from(error: CommandError) -> Self {
        Self::Refused {
            message: error.into_message(),
        }
    }
}

/// A saved file's new base: the token the next save of this draft carries.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SavedFile {
    pub disk_base: String,
}

/// Creates an empty file that must not exist yet, making its parent folders.
/// Anything already at the path (file, directory or link, tracked or ignored)
/// is [`FileWriteError::AlreadyExists`] and is left untouched, including when
/// it appears between the check and the publish.
pub fn sandbox_create(repo_path: &str, file_path: &str) -> Result<(), FileWriteError> {
    // The entry, not what it resolves to: a link at the path (dangling or not)
    // is something already there, and `Expect::Absent` must see the link.
    let repo = validate_repo(repo_path)?;
    let dest = sandbox_join_write_entry(&repo, file_path)?;
    crate::diff::write_regular_expecting(&dest, Expect::Absent, |_| Ok(Vec::new()))
        .map(|_| ())
        .map_err(|failure| FileWriteError::from_failure(failure, file_path, None))
}

/// Saves text that came out of the file editor. Unlike [`sandbox_write`],
/// which writes bytes it was given verbatim, this restores the file's CRLF
/// line endings the editor's textarea dropped, and refuses to overwrite a file
/// whose bytes the editor could not show faithfully (invalid UTF-8, or mixed
/// line endings it cannot keep per line).
pub fn sandbox_write_edited_text(
    repo_path: &str,
    file_path: &str,
    content: &str,
) -> Result<(), String> {
    sandbox_save_edited_text(repo_path, file_path, content, None)
        .map(|_| ())
        .map_err(|failure| failure.to_string())
}

/// [`sandbox_write_edited_text`] with the draft's base. `expected_base` is the
/// `disk_base` the editor was given when it opened the file (see
/// [`crate::diff::content_token`]); the save is refused as
/// [`FileWriteError::ChangedOnDisk`] when the file no longer has those bytes,
/// judged against the same read the line endings are chosen from and the swap
/// re-checks. `None` is an explicit overwrite.
pub fn sandbox_save_edited_text(
    repo_path: &str,
    file_path: &str,
    content: &str,
    expected_base: Option<&str>,
) -> Result<SavedFile, FileWriteError> {
    save_edited_text(repo_path, file_path, content, expected_base, false)
}

/// The editor's Overwrite: [`sandbox_save_edited_text`] for a user who chose
/// to keep their buffer over what is on disk. The encoder refuses a disk file
/// it cannot round-trip (invalid UTF-8, mixed line endings) to protect bytes
/// the save would destroy, but an overwrite discards the disk content by
/// intent, so when the encoder refuses the buffer is written byte-exactly as
/// it is. A disk file the encoder accepts still keeps its CRLF endings, and
/// the base check, the size limit and every path guard apply unchanged.
pub fn sandbox_overwrite_edited_text(
    repo_path: &str,
    file_path: &str,
    content: &str,
    expected_base: Option<&str>,
) -> Result<SavedFile, FileWriteError> {
    save_edited_text(repo_path, file_path, content, expected_base, true)
}

fn save_edited_text(
    repo_path: &str,
    file_path: &str,
    content: &str,
    expected_base: Option<&str>,
    overwrite: bool,
) -> Result<SavedFile, FileWriteError> {
    let dest = write_target(repo_path, file_path)?;
    let expect = expected_base.map_or(Expect::Any, Expect::Token);
    crate::diff::write_regular_expecting(&dest, expect, |existing| {
        use crate::engine::text_shape::{encode_edited_text_typed, EncodeRefusal};
        match encode_edited_text_typed(file_path, existing, content) {
            Ok(bytes) => Ok(bytes),
            // The two refusals an Overwrite discards by intent; any other
            // refusal added to the encoder is not bypassed by this arm.
            Err(EncodeRefusal::InvalidUtf8 { .. } | EncodeRefusal::MixedLineEndings { .. })
                if overwrite =>
            {
                Ok(content.as_bytes().to_vec())
            }
            Err(refusal) => Err(refusal.to_string()),
        }
    })
    .map(|disk_base| SavedFile { disk_base })
    .map_err(|failure| FileWriteError::from_failure(failure, file_path, expected_base))
}

/// Environment names git reads that can redirect it away from the repository
/// and settings the caller picked, or change what it prints. Every git child
/// has these removed ([`pin_git_environment`]), whichever builder spawned it.
///
/// - the repository pointers: where the index, objects, alternates, common dir,
///   namespace and shallow file live;
/// - the config channels, `GIT_CONFIG_PARAMETERS` being the shell-quoted one
///   (`'alias.st=!sh -c …'` outranks even repo-local config);
/// - transport and credential redirection: `GIT_SSH`, `GIT_SSH_COMMAND`,
///   `GIT_PROXY_COMMAND`, `GIT_ASKPASS` and friends;
/// - output and matching behaviour: `GIT_DIFF_OPTS` overrides `-U3`, and the
///   pathspec switches (`GIT_LITERAL_PATHSPECS`, `GIT_GLOB_PATHSPECS`,
///   `GIT_NOGLOB_PATHSPECS`, `GIT_ICASE_PATHSPECS`) retarget the `:(literal)`
///   pathspecs the writers rely on, so `stage f*.txt` could touch other files;
/// - `GIT_ATTR_SOURCE`, which swaps the attributes diffs and filters see.
const STRIPPED_GIT_ENV: &[&str] = &[
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_COMMON_DIR",
    "GIT_NAMESPACE",
    "GIT_SHALLOW_FILE",
    "GIT_ATTR_SOURCE",
    "GIT_CONFIG",
    "GIT_CONFIG_GLOBAL",
    "GIT_CONFIG_SYSTEM",
    "GIT_CONFIG_COUNT",
    "GIT_CONFIG_PARAMETERS",
    "GIT_EXTERNAL_DIFF",
    "GIT_SEQUENCE_EDITOR",
    "GIT_EXEC_PATH",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_SSH_VARIANT",
    "GIT_PROXY_COMMAND",
    "GIT_ASKPASS",
    "GIT_DIFF_OPTS",
    "GIT_LITERAL_PATHSPECS",
    "GIT_GLOB_PATHSPECS",
    "GIT_NOGLOB_PATHSPECS",
    "GIT_ICASE_PATHSPECS",
];

/// Families of names removed by prefix: the numbered config channel, credential
/// helper overrides, and every tracing switch (`GIT_TRACE`, `GIT_TRACE2*`,
/// `GIT_TRACE_PACKET`, ...), whose output floods stderr and pushes the fatal
/// line out of the few bytes of it that are kept.
const STRIPPED_GIT_ENV_PREFIXES: &[&str] = &[
    "GIT_CONFIG_KEY_",
    "GIT_CONFIG_VALUE_",
    "GIT_CREDHELPER",
    "GIT_TRACE",
];

/// True for inherited environment names [`pin_git_environment`] removes.
fn is_injected_git_env(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    STRIPPED_GIT_ENV.contains(&upper.as_str())
        || STRIPPED_GIT_ENV_PREFIXES
            .iter()
            .any(|prefix| upper.starts_with(prefix))
}

/// Config every git child is run with, ahead of its subcommand, so a
/// developer's own settings cannot change what the parsers read:
/// `core.quotepath` keeps non-ASCII paths as raw bytes; `log.showSignature`
/// prints a "No signature" line (and runs gpg) in front of every commit id;
/// the colour settings put ANSI escapes in diffs (the specific keys outrank
/// `color.ui`, so they are pinned too); `diff.noprefix` and
/// `diff.mnemonicPrefix` change the `a/` `b/` prefixes the diff parser keys on.
const PINNED_GIT_CONFIG: &[&str] = &[
    "core.quotepath=false",
    "log.showSignature=false",
    "color.ui=never",
    "color.diff=never",
    "color.status=never",
    "color.branch=never",
    "diff.noprefix=false",
    "diff.mnemonicPrefix=false",
    // A user's `status.showUntrackedFiles=no` would hide a new file from every
    // status read, and `normal` collapses a new directory to `dir/`. Both make
    // a file another worktree is holding invisible to a collision check.
    "status.showUntrackedFiles=all",
];

/// The one porcelain status read: NUL-separated records, raw UTF-8 paths, and
/// every untracked file listed individually (`-u`) rather than collapsed into
/// its directory. `GitReader::STATUS_ARGV` and the worktree scans both read
/// status through this, so a collision check and a file list cannot disagree
/// about what a new directory contains. A pathspec goes after a trailing `--`.
pub const STATUS_ARGV: &[&str] = &[
    "-c",
    "core.quotepath=off",
    "status",
    "--porcelain=v1",
    "-u",
    "-z",
];

/// Subcommands that print a patch, where `diff.external` (a config key, which
/// no `-c` can blank) would run a program instead of git's own diff.
const PATCH_SUBCOMMANDS: &[&str] = &["diff", "show", "log"];

/// The patch subcommands that print a signature header (and run the verifier
/// to do it) when `log.showSignature` or `--show-signature` asks. `--no-show-signature`
/// rides every one of them from here, so no read has to remember it; `diff` is
/// left out because it rejects the flag. It turns off that header only: it does
/// not touch the `%G*` placeholders, which always verify, and those are kept
/// out of every bulk format string by a source-scan test.
const SIGNATURE_PRINTING_SUBCOMMANDS: &[&str] = &["show", "log"];

/// The one place the environment of a git child is decided. Both spawn
/// builders ([`git_command_with_env`] for git itself and
/// [`build_capture_command_in`] for tools that shell out to git, such as `gh`)
/// call it, so they cannot drift apart: a setting added here reaches both.
///
/// Prompts and pagers are off, the locale is `C` (the parsers match git's
/// English words), optional locks are skipped so a read never contends with a
/// writer, every name in [`STRIPPED_GIT_ENV`] and [`STRIPPED_GIT_ENV_PREFIXES`]
/// is removed, and the editors are pinned to `true`.
///
/// Editors are neutralized AFTER the strip, which would otherwise remove
/// `GIT_SEQUENCE_EDITOR` right back out again. Removal alone is not enough:
/// with none set, git falls back to `core.editor` and then to vi, and any
/// subcommand that wants a message (`rebase --continue`, `am --continue`,
/// `merge --continue`, a bare `commit`) launches it against a null stdin and
/// blocks until the command timeout — surfacing as "git timed out" rather than
/// a result. `true` exits 0 immediately, so git accepts the message it
/// prepared. Every message-bearing call here already passes `-m` or
/// `--no-edit`, so no existing caller's behaviour changes.
fn pin_git_environment(cmd: &mut Command) {
    cmd.env("GIT_TERMINAL_PROMPT", "0")
        .env("GCM_INTERACTIVE", "never")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("LC_ALL", "C");
    for name in STRIPPED_GIT_ENV {
        cmd.env_remove(name);
    }
    for (name, _) in std::env::vars_os() {
        if is_injected_git_env(&name.to_string_lossy()) {
            cmd.env_remove(&name);
        }
    }
    cmd.env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true");
}

/// Reads `PATH`/`HOME` from the process and defers to
/// [`git_command_with_env`], mirroring the [`capture_command`] /
/// [`capture_command_with_env`] split so the environment stays injectable for
/// tests instead of being read from under them.
fn git_command(repo: Option<&Path>, args: &[&str]) -> Command {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    git_command_with_env(repo, args, path_var.as_deref(), home.as_deref())
}

fn git_command_with_env(
    repo: Option<&Path>,
    args: &[&str],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Command {
    let child_path = (!cfg!(windows))
        .then(|| extended_child_path(path_var, home))
        .flatten();
    // Rust falls back to fork when PATH is changed and the program is a
    // bare name. Resolve against precisely the child's search path to retain
    // posix_spawn on macOS, without lossy conversion or a permanent cache
    // that would ignore a replaced/removed executable. Relative entries need
    // the OS's child-cwd semantics and deliberately retain the fallback.
    let program = child_path.as_deref().and_then(|path| {
        let dirs: Vec<PathBuf> = std::env::split_paths(path).collect();
        dirs.iter()
            .all(|dir| dir.is_absolute())
            .then(|| find_in_dirs("git", &dirs))
            .flatten()
    });
    #[cfg(target_os = "macos")]
    let program = program.map(|resolved| {
        past_xcrun_git_shim(
            resolved,
            &REAL_GIT_DIRS.map(PathBuf::from),
            &developer_dirs(),
        )
    });
    let mut cmd = Command::new(program.as_deref().unwrap_or_else(|| Path::new("git")));
    // The pinned config goes ahead of the caller's arguments, so a `-c` the
    // caller passes itself still wins; `--no-ext-diff` goes right after the
    // subcommand of a patch-printing command.
    for pin in PINNED_GIT_CONFIG {
        cmd.args(["-c", pin]);
    }
    match subcommand_index(args) {
        Some(at) if PATCH_SUBCOMMANDS.contains(&args[at]) => {
            cmd.args(&args[..=at]).arg("--no-ext-diff");
            if SIGNATURE_PRINTING_SUBCOMMANDS.contains(&args[at]) {
                cmd.arg("--no-show-signature");
            }
            cmd.args(&args[at + 1..])
        }
        _ => cmd.args(args),
    };
    pin_git_environment(&mut cmd);
    if NO_LAZY_FETCH.get() {
        // Set after the scrub, like the other pins: a missing object of a
        // partial clone is then an error instead of a fetch from the remote.
        cmd.env("GIT_NO_LAZY_FETCH", "1");
    }
    // Git resolves its own helpers through the child's PATH, so a GUI launch
    // handed them the same minimal `/usr/bin:/bin:/usr/sbin:/sbin` that hid
    // `gh` and `cargo` from us: `gpg` for a signed commit, the interpreter a
    // `pre-commit` hook shells out to (husky's `npx`, and `node` behind it),
    // `git-lfs`, and any external diff/merge tool. Each failure surfaced as
    // git's own error — "cannot run gpg", a hook exiting 127 — which reads as
    // a broken repository rather than a PATH the app chose.
    //
    // Set after the strip loop for the same reason the editors are: the loop
    // only removes `GIT_*` names, but keeping every environment decision in
    // one place after it is what stops the next added name from being quietly
    // undone. Inherited entries stay ahead of the appended ones, so no helper
    // that already resolved starts resolving somewhere else.
    if let Some(child_path) = child_path {
        cmd.env("PATH", child_path);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = repo {
        cmd.current_dir(dir);
    }
    cmd
}

/// Runs `git` in `repo` with a hard timeout and bounded stdout/stderr.
///
/// The timeout is [`DEFAULT_TIMEOUT`], or [`HOOK_TIMEOUT`] for a subcommand
/// that runs repository hooks (`commit`, `merge`, `rebase`, `am`, …): a
/// slow `pre-commit` is the repository owner's program working, not git
/// hanging.
pub fn git(repo: &Path, args: &[&str]) -> Result<Vec<u8>, CommandError> {
    git_timeout(
        Some(repo),
        args,
        hooks::default_timeout_for(subcommand(args)),
        None,
    )
}

pub fn git_text(repo: &Path, args: &[&str]) -> Result<String, CommandError> {
    let bytes = git(repo, args)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Like [`git_text`], but an over-cap stream is data rather than an error:
/// returns what arrived plus the truncation flag. For callers whose input
/// tolerates a prefix (coverage family detection), where failing the whole
/// scan would be worse than reading part of the listing.
pub fn git_text_partial(repo: &Path, args: &[&str]) -> Result<(String, bool), CommandError> {
    let (bytes, incomplete) = git_run(Some(repo), args, DEFAULT_TIMEOUT, None)?;
    Ok((
        String::from_utf8_lossy(&bytes).into_owned(),
        incomplete.is_some(),
    ))
}

/// Runs `git` with an explicit stdout budget, returning the text plus whether
/// the stream was cut off at the cap.
///
/// The seam every UI-bound payload goes through. Truncation is data, not an
/// error: a diff too large to render is still worth showing the head of, so
/// long as the caller says so rather than presenting a prefix as the whole
/// thing. See [`crate::engine::budget`] for the budgets themselves.
pub fn git_text_capped(
    repo: &Path,
    args: &[&str],
    cap: usize,
) -> Result<(String, Option<Incomplete>), CommandError> {
    let (bytes, incomplete) = git_bytes_capped(repo, args, cap)?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), incomplete))
}

/// [`git_text_capped`] without the UTF-8 decode: git's exact stdout bytes plus
/// the truncation flag. For the one reader whose consumer defines its input as
/// bytes (the Lappi caller contract), where a lossy decode would substitute
/// U+FFFD for what git printed while still reporting a matching length.
pub fn git_bytes_capped(
    repo: &Path,
    args: &[&str],
    cap: usize,
) -> Result<(Vec<u8>, Option<Incomplete>), CommandError> {
    git_run_capped(Some(repo), args, DEFAULT_TIMEOUT, None, cap)
}

/// Runs `git` in `repo` and hands back the finished run *whatever* its exit
/// status, through the same spawn gate, timeout, environment scrub and output
/// caps as [`git`].
///
/// For the callers where a non-zero status is an answer rather than a failure:
/// `git notes show` exits 1 to say "this object has no note", which is data,
/// while failing to spawn at all is not. [`git`] flattens both into one
/// `Err(String)`, and a caller that cannot tell them apart ends up reporting a
/// commit it could not look at as a commit it looked at and found nothing on.
pub(crate) fn git_captured(repo: &Path, args: &[&str]) -> Result<BoundedRun, CommandError> {
    git_captured_inner(repo, args, None, MAX_OUTPUT_BYTES)
}

/// [`git_captured`] with `stdin_bytes` fed to the child, for git's `--batch`
/// style line protocols.
pub(crate) fn git_captured_with_stdin(
    repo: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<BoundedRun, CommandError> {
    git_captured_inner(repo, args, Some(stdin_bytes), MAX_OUTPUT_BYTES)
}

/// What a [`read_objects_batch`] caller expects to find at each id: the object
/// type git must report, and the noun its error messages use for it.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BatchObject {
    pub kind: &'static str,
    pub noun: &'static str,
}

/// Ids more than this many in one `cat-file --batch` question. A caller
/// already bounds its list (the pulse walk by `MAX_PULSE_COMMITS`); this is
/// the reader's own ceiling, so no caller can hand it an unbounded stdin.
const MAX_BATCH_OBJECTS: usize = 100_000;

/// Reads objects in one `git cat-file --batch`, where each used to cost its
/// own process, and maps each one's bytes through `extract`.
///
/// Every answer is taken by the size git declares, never by lines: an object
/// may hold any byte, newlines included. A missing object, one of another
/// type, an answer that names another id, and a stream that stops short are
/// each an `Err` for the objects they leave unread, never an empty value. A
/// stream cut at `budget` (or by the deadline) is read as far as it is whole:
/// the objects before the cut keep their answers, and every later one is an
/// `Err` that says the output was cut.
pub(crate) fn read_objects_batch<T: Clone>(
    repo: &Path,
    oids: &[String],
    object: BatchObject,
    budget: usize,
    extract: &dyn Fn(&[u8]) -> T,
) -> HashMap<String, Result<T, CommandError>> {
    let mut answers: HashMap<String, Result<T, CommandError>> = HashMap::new();
    // Git answers with an object's lowercase name whatever case it was asked
    // by, and the stream is matched to its questions by that name, so every
    // question is lowercased; the answer is given back under the spelling the
    // caller used too.
    let mut respelled: Vec<(&String, String)> = Vec::new();
    let mut asked: HashSet<String> = HashSet::new();
    let mut sent: Vec<String> = Vec::new();
    let mut query = String::new();
    for oid in oids {
        let shaped = matches!(oid.len(), 40 | 64) && oid.bytes().all(|b| b.is_ascii_hexdigit());
        if !shaped {
            answers
                .entry(oid.clone())
                .or_insert_with(|| Err(format!("{oid:?} is not a {} id", object.noun).into()));
            continue;
        }
        let lower = oid.to_ascii_lowercase();
        if lower != *oid {
            respelled.push((oid, lower.clone()));
        }
        if !asked.insert(lower.clone()) {
            continue;
        }
        if sent.len() == MAX_BATCH_OBJECTS {
            answers.insert(
                lower,
                Err(format!("more than {MAX_BATCH_OBJECTS} objects were asked for at once").into()),
            );
            continue;
        }
        query.push_str(&lower);
        query.push('\n');
        sent.push(lower);
    }
    if !sent.is_empty() {
        let questions: Vec<&str> = sent.iter().map(String::as_str).collect();
        fetch_objects(
            repo,
            &query,
            &questions,
            object,
            budget,
            extract,
            &mut answers,
        );
    }
    for (original, lower) in respelled {
        if let Some(answer) = answers.get(&lower).cloned() {
            answers.insert(original.clone(), answer);
        }
    }
    answers
}

/// Runs one `cat-file --batch` for `questions` (lowercase ids, `query` is the
/// same list one per line) and records an answer for each in `answers`.
fn fetch_objects<T>(
    repo: &Path,
    query: &str,
    sent: &[&str],
    object: BatchObject,
    budget: usize,
    extract: &dyn Fn(&[u8]) -> T,
    answers: &mut HashMap<String, Result<T, CommandError>>,
) {
    let run = git_captured_inner(
        repo,
        &["cat-file", "--batch"],
        Some(query.as_bytes()),
        budget,
    );
    let run = match run {
        Ok(run) => run,
        Err(e) => {
            mark_unread(
                answers,
                sent,
                e.wrap(|e| format!("could not run git cat-file: {e}")),
            );
            return;
        }
    };
    // A stream cut at the budget or the deadline is a prefix whose child was
    // stopped, so it exits unsuccessfully: it is read as far as it is whole
    // instead of being discarded as a failed run.
    if !run.success && run.incomplete.is_none() {
        let stderr = String::from_utf8_lossy(&run.stderr).trim().to_string();
        mark_unread(
            answers,
            sent,
            format!("git cat-file --batch failed: {stderr}").into(),
        );
        return;
    }
    let cut = run
        .incomplete
        .as_ref()
        .map(|reason| format!("git cat-file {}", reason.describe()));
    read_batch_answers(&run.stdout, sent, object, extract, cut.as_deref(), answers);
}

/// Where a body of the declared `size` starting at `pos` ends, if the stream
/// holds all of it and the newline git writes after it. A size is the
/// stream's own claim, so it is never trusted to fit in `usize` arithmetic.
fn body_end(out: &[u8], pos: usize, size: &str) -> Option<usize> {
    let end = pos.checked_add(size.parse::<usize>().ok()?)?;
    (out.get(end) == Some(&b'\n')).then_some(end)
}

/// Marks every object in `rest` unread for the same reason.
fn mark_unread<T>(
    answers: &mut HashMap<String, Result<T, CommandError>>,
    rest: &[&str],
    why: CommandError,
) {
    for oid in rest {
        answers.insert((*oid).to_string(), Err(why.clone()));
    }
}

/// Splits a `cat-file --batch` stream into the answers to `sent`, in order.
/// Once the stream is out of step with its questions, nothing after that
/// point is attributed to any of them. `cut` is why the stream ended early,
/// when it did, and is what a question left unanswered by it reports.
pub(crate) fn read_batch_answers<T>(
    out: &[u8],
    sent: &[&str],
    object: BatchObject,
    extract: &dyn Fn(&[u8]) -> T,
    cut: Option<&str>,
    answers: &mut HashMap<String, Result<T, CommandError>>,
) {
    let noun = object.noun;
    let mut pos = 0usize;
    for (index, oid) in sent.iter().enumerate() {
        let Some(end) = out[pos..].iter().position(|&b| b == b'\n') else {
            let why = cut.map_or_else(
                || format!("git cat-file answered nothing for this {noun}"),
                |cut| format!("{cut} before this {noun} was read"),
            );
            mark_unread(answers, &sent[index..], why.into());
            return;
        };
        let header = String::from_utf8_lossy(&out[pos..pos + end]).into_owned();
        pos += end + 1;
        let fields: Vec<&str> = header.split(' ').collect();
        if fields.first() != Some(oid) {
            // Every later answer is now out of step with its question.
            mark_unread(
                answers,
                &sent[index..],
                format!("git cat-file answered {header:?} for {noun} {oid}").into(),
            );
            return;
        }
        let answer = match fields.as_slice() {
            [_, kind, size] if *kind == object.kind => match body_end(out, pos, size) {
                Some(end) => {
                    let content = extract(&out[pos..end]);
                    pos = end + 1;
                    Ok(content)
                }
                None => {
                    let why = cut.map_or_else(
                        || format!("git cat-file cut the {noun} {oid} short"),
                        |cut| format!("{cut} inside the {noun} {oid}"),
                    );
                    mark_unread(answers, &sent[index..], why.into());
                    return;
                }
            },
            [_, "missing"] => {
                Err(format!("the {noun} {oid} is missing from this repository").into())
            }
            [_, kind, size] => match body_end(out, pos, size) {
                // Skipped by its size so the next answer stays in step.
                Some(end) => {
                    pos = end + 1;
                    Err(format!("{oid} is a {kind}, not a {noun}").into())
                }
                None => {
                    mark_unread(
                        answers,
                        &sent[index..],
                        format!("git cat-file answered {header:?}").into(),
                    );
                    return;
                }
            },
            _ => Err(format!("git cat-file answered {header:?} for {noun} {oid}").into()),
        };
        answers.insert((*oid).to_string(), answer);
    }
}

fn git_captured_inner(
    repo: &Path,
    args: &[&str],
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
) -> Result<BoundedRun, CommandError> {
    crate::repository_trust::require(repo)?;
    let label = format!("git {}", subcommand(args));
    run_bounded_capped(
        git_command(Some(repo), args),
        &label,
        DEFAULT_TIMEOUT,
        stdin_bytes,
        stdout_cap,
    )
}

/// [`git_captured`] with a caller-owned observer, timeout and stdout cap: the
/// observer sees output as it streams and can stop the child (its
/// `cancelled()`), which is how a search halts at its match limit or on a
/// user cancel instead of reading to the end. Same trust check, environment
/// scrub and spawn gate as every other git read; the finished run comes back
/// whatever its exit status, with `cancelled` and `incomplete` set.
pub(crate) fn git_observed(
    repo: &Path,
    args: &[&str],
    timeout: Duration,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
) -> Result<BoundedRun, CommandError> {
    crate::repository_trust::require(repo)?;
    let label = format!("git {}", subcommand(args));
    let mut cmd = git_command(Some(repo), args);
    let _keep_deadline_prefix = KeepDeadlinePrefix::enter();
    run_observed(&mut cmd, &label, timeout, None, stdout_cap, observer)
}

/// A repository-less git (`clone`) whose output streams to `observer` as it
/// is written: how a clone reports progress instead of being a silent wait.
/// The finished run comes back whatever its exit status; a deadline is an
/// error, as for [`git_global_with_timeout`].
pub(crate) fn git_global_observed(
    args: &[&str],
    timeout: Duration,
    observer: &mut dyn ProcessObserver,
) -> Result<BoundedRun, CommandError> {
    let label = format!("git {}", subcommand(args));
    let mut cmd = git_command(None, args);
    run_observed(&mut cmd, &label, timeout, None, MAX_OUTPUT_BYTES, observer)
}

pub fn git_global(args: &[&str]) -> Result<Vec<u8>, CommandError> {
    git_timeout(None, args, DEFAULT_TIMEOUT, None)
}

pub fn git_with_stdin(
    repo: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<Vec<u8>, CommandError> {
    git_timeout(
        Some(repo),
        args,
        hooks::default_timeout_for(subcommand(args)),
        Some(stdin_bytes),
    )
}

/// A caller-owned index transaction. The override is applied only after the
/// normal environment scrub; inherited GIT_INDEX_FILE remains untrusted.
pub(crate) fn git_with_index(
    repo: &Path,
    index: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
) -> Result<Vec<u8>, CommandError> {
    crate::repository_trust::require(repo)?;
    let mut command = git_command(Some(repo), args);
    // Canonical paths carry the Windows verbatim prefix, which Git rejects
    // in GIT_INDEX_FILE. The transaction may create this file, so do not
    // canonicalize it again or require it to exist before read-tree.
    #[cfg(windows)]
    let plain_index = index.to_str().and_then(simplified_windows_path);
    #[cfg(windows)]
    let index = plain_index.as_deref().map(Path::new).unwrap_or(index);
    command.env("GIT_INDEX_FILE", index);
    let output = run_bounded(
        command,
        "git index transaction",
        DEFAULT_TIMEOUT,
        Some(stdin_bytes),
    )?;
    if let Some(reason) = output.incomplete {
        return Err(format!("Index transaction output {}", reason.describe()).into());
    }
    if !output.success {
        return Err(format!(
            "Index transaction failed: {}",
            String::from_utf8_lossy(&output.stderr[..output.stderr.len().min(2000)])
        )
        .into());
    }
    Ok(output.stdout)
}

/// [`git_captured`] with the object database redirected: new objects are
/// written to `object_dir` and existing ones are read through `alternates`.
///
/// The redirect is applied after the environment scrub (an inherited
/// `GIT_OBJECT_DIRECTORY` stays untrusted) and is the only override this seam
/// takes, so the quarantine in [`crate::engine::object_quarantine`] is the one
/// way a caller points git's object writes anywhere but the repository. The
/// same trust check, spawn gate and output caps as [`git_captured`] apply; the
/// deadline is the caller's.
pub(crate) fn git_captured_quarantined(
    repo: &Path,
    args: &[&str],
    object_dir: &Path,
    alternates: &std::ffi::OsStr,
    timeout: Duration,
) -> Result<BoundedRun, CommandError> {
    crate::repository_trust::require(repo)?;
    let label = format!("git {}", subcommand(args));
    let mut command = git_command(Some(repo), args);
    command.env("GIT_OBJECT_DIRECTORY", object_dir);
    command.env("GIT_ALTERNATE_OBJECT_DIRECTORIES", alternates);
    run_bounded(command, &label, timeout, None)
}

/// Runs `git` in `repo` with an explicit deadline instead of
/// [`DEFAULT_TIMEOUT`].
///
/// Use for network-bound work (`clone`, `fetch`, `push`) where a multi-gigabyte
/// transfer is legitimate and a short default cap would kill healthy traffic.
pub fn git_with_timeout(
    repo: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, CommandError> {
    git_timeout(Some(repo), args, timeout, None)
}

/// Like [`git_with_timeout`], but for repo-less global invocations.
pub fn git_global_with_timeout(args: &[&str], timeout: Duration) -> Result<Vec<u8>, CommandError> {
    git_timeout(None, args, timeout, None)
}

/// [`git_with_timeout`] with UTF-8 (lossy) text output.
pub fn git_text_with_timeout(
    repo: &Path,
    args: &[&str],
    timeout: Duration,
) -> Result<String, CommandError> {
    let bytes = git_with_timeout(repo, args, timeout)?;
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Runs network-bound git plumbing in `repo` under [`NETWORK_TIMEOUT`].
pub fn git_text_network(repo: &Path, args: &[&str]) -> Result<String, CommandError> {
    git_text_with_timeout(repo, args, NETWORK_TIMEOUT)
}

/// Captured stdout/stderr from a bounded external process.
///
/// Unlike `run_command`, a non-zero exit is not an error: tools such as
/// `npm audit` and `npm outdated` use the status code to mean "findings",
/// and the JSON the caller needs is still on stdout.
#[derive(Debug, Clone)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
}

impl CapturedOutput {
    pub fn stdout_text(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub fn stderr_text(&self) -> String {
        String::from_utf8_lossy(&self.stderr).trim().to_string()
    }
}

/// Runs an external command with the same timeout and output caps as `git`.
pub fn run_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<Vec<u8>, CommandError> {
    run_command_in(program, args, timeout, None)
}

/// Like [`run_command`], with an optional working directory (used for `gh` in a repo).
pub fn run_command_in(
    program: &str,
    args: &[&str],
    timeout: Duration,
    cwd: Option<&Path>,
) -> Result<Vec<u8>, CommandError> {
    let output = capture_command(program, args, cwd, timeout, &[])?;
    if output.success {
        return Ok(output.stdout);
    }
    let err = output.stderr_text();
    if err.is_empty() {
        return Err(format!("{} failed with status {}", program, output.status_code).into());
    }
    Err(err.into())
}

/// Fallback directories searched when a bare tool name is missing from the
/// inherited `PATH`, and appended to the child's PATH so nested lookups (a
/// shebang script's interpreter, `govulncheck` shelling out to `go`) resolve
/// the same way.
///
/// GUI launches on macOS (Finder, Dock, `open`) hand the app a minimal PATH
/// (`/usr/bin:/bin:/usr/sbin:/sbin`) that omits Homebrew and user-local bin
/// directories, so `Command::new("gh")` failed to resolve even though the CLI
/// was installed — every GitHub view then reported "`gh` is not installed".
/// Terminal launches never saw this. Superset of the convention mirrored from
/// [`crate::harness::sidecar::resolve_binary`] with Go and Rust toolchain
/// locations for scanners that spawn `go` or `cargo` themselves; nonexistent
/// entries are harmlessly skipped.
///
/// `CARGO_HOME` is deliberately not consulted for the Rust entry. A custom
/// value is set in a shell profile, and a GUI launch inherits no shell
/// profile — so in the launch this whole mechanism exists for, reading it
/// would find nothing. In the launches where it *is* set, PATH was inherited
/// too and resolution never reaches this list. `~/.cargo/bin` is rustup's
/// default and the only spelling reachable here.
fn gui_launch_fallback_dirs(home: Option<&std::ffi::OsStr>) -> Vec<PathBuf> {
    let mut dirs = vec![
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/usr/local/bin"),
    ];
    if let Some(home) = home {
        dirs.push(PathBuf::from(home).join(".local/bin"));
        // Grok Build installs the real binary under `~/.grok/bin` and may
        // only symlink `grok` into `~/.local/bin`. A GUI PATH that sees
        // neither Homebrew nor that symlink still has to find it.
        dirs.push(PathBuf::from(home).join(".grok/bin"));
    }
    // Fixed system path, so it does not depend on knowing `home` — gating it
    // on that withheld a still-valid directory from launchd/daemon contexts,
    // which are exactly the ones with no inherited PATH. Kept in its original
    // position so no existing precedence between these dirs shifts.
    dirs.push(PathBuf::from("/usr/local/go/bin"));
    if let Some(home) = home {
        // Standard GOBIN/GOPATH install location (`go install ...` default).
        dirs.push(PathBuf::from(home).join("go/bin"));
        // rustup's install root: `cargo`, `rustc`, `rustup` and every
        // `cargo-*` subcommand binary (`cargo-audit`, `cargo-llvm-cov`) live
        // here and nowhere a GUI-launch PATH can see.
        dirs.push(PathBuf::from(home).join(".cargo/bin"));
    }
    dirs
}

/// Whose `PATH` a spawn was handed, which decides whether the
/// [`gui_launch_fallback_dirs`] may widen it.
///
/// The fallback dirs repair an *inherited* PATH that a GUI launch left
/// minimal. A PATH a caller chose on purpose — a test simulating a host with
/// a tool absent — is the whole answer, and widening it made the choice
/// meaningless: a scan told "cargo-deny is not on this PATH" still found
/// `/opt/homebrew/bin/cargo-deny` on any host that had one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PathSource {
    /// Read from the process environment: searched first, then the fallback
    /// dirs, and the child's PATH is extended the same way.
    Inherited,
    /// Supplied by the caller: searched alone, and handed to the child as is.
    Explicit,
}

/// Directories a bare program name is searched in, in precedence order.
///
/// Single owner of that list for resolution ([`resolve_spawn_program_in`],
/// [`find_external_tool_with`]). Empty entries are dropped: POSIX reads one as
/// "the current directory".
fn tool_search_dirs(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| !entry.as_os_str().is_empty())
        .collect();
    if source == PathSource::Inherited {
        dirs.extend(gui_launch_fallback_dirs(home));
    }
    dirs
}

/// First entry of `dirs` holding a spawnpable file named `program`.
///
/// Deliberately stricter than an `is_file()` scan so resolution matches what
/// the OS PATH walk would have done:
/// - relative directory entries are ignored. POSIX reads an empty entry as
///   "the current directory", and a relative candidate would be resolved by
///   the spawn against the child's post-`chdir` working directory (`cwd`
///   argument), silently searching the wrong tree;
/// - on Unix the candidate must carry an execute bit — `execvp` skips
///   non-executable matches rather than failing the whole lookup, and picking
///   one would turn "tool found" into a PermissionDenied spawn error;
/// - broken symlinks and directories named like the tool are skipped;
/// - on Windows the suffixed spellings are tried BEFORE the bare name, in the
///   order the OS itself uses, and a name that already carries one of those
///   suffixes is never double-suffixed.
fn find_in_dirs(program: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    let names = spawn_candidate_names(program);
    dirs.iter().filter(|dir| dir.is_absolute()).find_map(|dir| {
        names
            .iter()
            .map(|name| dir.join(name))
            .find(|candidate| is_executable_file(candidate))
    })
}

/// Apple's `/usr/bin/git` is not git. It is the `xcrun` shim, which works out
/// the active developer directory and then execs the real binary under it, on
/// every call. `benches/process_spawn.rs` times `git rev-parse HEAD` with the
/// child environment set here, interleaved: on an M-series host the shim's
/// p50 was 7.9 ms, Homebrew's git 4.2 ms (-47%) and the developer-tools
/// binary the shim execs 3.3 ms (-59%). Homebrew's is preferred anyway: it is
/// the git a user installed on purpose, and usually newer. Every git read
/// GitPulse makes is a spawn, so the shim roughly doubled the cost of each.
/// SIP keeps `/usr/bin` read-only, so this path is always the shim.
#[cfg(target_os = "macos")]
const XCRUN_GIT_SHIM: &str = "/usr/bin/git";

/// Package-manager directories whose `git` is preferred over the shim:
/// Homebrew on Apple silicon, then Homebrew on Intel and other `/usr/local`
/// installs. Deliberately not every GUI fallback directory — a stray `git` in
/// `~/.cargo/bin` or `~/go/bin` is not one the user chose as their git.
#[cfg(target_os = "macos")]
const REAL_GIT_DIRS: [&str; 2] = ["/opt/homebrew/bin", "/usr/local/bin"];

/// The developer directories `xcrun` itself would consult, most specific
/// first: `DEVELOPER_DIR`, then the `xcode-select` choice (a symlink, so no
/// spawn is needed to read it), then the Command Line Tools default.
#[cfg(target_os = "macos")]
fn developer_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(dir) = std::env::var_os("DEVELOPER_DIR").filter(|dir| !dir.is_empty()) {
        dirs.push(PathBuf::from(dir));
    }
    if let Ok(dir) = std::fs::read_link("/var/db/xcode_select_link") {
        dirs.push(dir);
    }
    dirs.push(PathBuf::from("/Library/Developer/CommandLineTools"));
    dirs
}

/// `resolved`, unless it is the `xcrun` shim and a real git is installed:
/// then a package-manager git from `real_git_dirs`, else the binary the shim
/// would have exec'd from the first of `developer_dirs` that holds one. A
/// git found earlier on `PATH` than `/usr/bin` is the user's choice and is
/// returned untouched; so is the shim when no real git exists, because then
/// the shim is what offers to install the developer tools.
#[cfg(target_os = "macos")]
fn past_xcrun_git_shim(
    resolved: PathBuf,
    real_git_dirs: &[PathBuf],
    developer_dirs: &[PathBuf],
) -> PathBuf {
    let shim = Path::new(XCRUN_GIT_SHIM);
    if resolved != shim {
        return resolved;
    }
    let packaged = real_git_dirs.iter().map(|dir| dir.join("git"));
    // `DEVELOPER_DIR` may name the `Xcode.app` bundle as well as its
    // `Contents/Developer`; `xcrun` accepts both, so both are tried.
    let developer = developer_dirs.iter().flat_map(|dir| {
        [
            dir.join("usr/bin/git"),
            dir.join("Contents/Developer/usr/bin/git"),
        ]
    });
    // A candidate that is a link back to the shim would cost exactly what the
    // shim does, so it is judged by where it leads, not by its own name.
    packaged
        .chain(developer)
        .filter(|candidate| candidate.is_absolute() && is_executable_file(candidate))
        .find(|candidate| std::fs::canonicalize(candidate).is_ok_and(|target| target != shim))
        .unwrap_or(resolved)
}

/// Executable suffixes tried on Windows, in the order `CreateProcess`'
/// own `PATHEXT` walk uses them.
///
/// A fixed list rather than the environment's `PATHEXT`: the default value of
/// that variable also carries `.VBS`, `.JS`, `.WSF` and friends, which are
/// Windows Script Host inputs. Resolving a tool name to one of those would
/// hand an interpreter a script this process never meant to run, and no tool
/// GitPulse spawns ships as one.
#[cfg(windows)]
const WINDOWS_EXECUTABLE_SUFFIXES: &[&str] = &[".com", ".exe", ".bat", ".cmd"];

/// Names to try for `program` in one directory, most specific first.
///
/// On Windows the bare name is tried LAST, not first. Node installs both
/// `npm.cmd` and an extension-less `npm` — a shell script for Git Bash — in
/// the same directory, and `is_executable_file` counts any regular file there.
/// Trying the bare name first therefore resolved the shell script and handed
/// it to `CreateProcess`, which fails with "%1 is not a valid Win32
/// application" (os error 193). The OS PATH walk would have found `npm.cmd`.
///
/// SECURITY: this makes `.bat`/`.cmd` resolvable, so a spawn can now reach a
/// batch file. It does not introduce shell interpretation: `std::process`
/// detects a batch target, routes it through `cmd.exe` itself, and quotes the
/// arguments with cmd's own rules — returning `InvalidInput` for an argument
/// it cannot safely escape rather than emitting a mis-quoted command line
/// (the CVE-2024-24576 fix, present since 1.77.2). Building a `cmd /c` line
/// here by hand is what would have added that surface.
fn spawn_candidate_names(program: &str) -> Vec<String> {
    #[cfg(windows)]
    {
        let lower = program.to_ascii_lowercase();
        if WINDOWS_EXECUTABLE_SUFFIXES
            .iter()
            .any(|suffix| lower.ends_with(suffix))
        {
            return vec![program.to_string()];
        }
        let mut names: Vec<String> = WINDOWS_EXECUTABLE_SUFFIXES
            .iter()
            .map(|suffix| format!("{program}{suffix}"))
            .collect();
        names.push(program.to_string());
        names
    }
    #[cfg(not(windows))]
    {
        vec![program.to_string()]
    }
}

/// True when `path` is a regular, executable file (symlinks followed; broken
/// links, directories and non-executable files are false).
#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

/// Windows counterpart: any regular file counts (extension handling is the
/// caller's job via [`find_in_dirs`]' name list).
#[cfg(windows)]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Longest inherited `PATH` [`extended_child_path`] will still append to.
///
/// `argv` and `envp` share one buffer and `execve` fails the whole spawn with
/// `E2BIG` once it is full — 1 MiB on this project's macOS hosts, measured by
/// the test that pins this rather than taken from a manual. A PATH anywhere
/// near that is pathological, but appending to it is how a helpful change
/// becomes the reason a spawn that used to work stops working, and the failure
/// arrives as "Failed to spawn git: Argument list too long" with nothing
/// pointing back here. Below this ceiling the handful of short fallback dirs
/// cannot move the total meaningfully; at or above it the child keeps exactly
/// the environment it would have inherited without us.
const MAX_INHERITED_PATH_BYTES: usize = 64 * 1024;

/// Child-side `PATH` value: inherited entries first (precedence preserved),
/// then the [`gui_launch_fallback_dirs`] that are not already present.
///
/// Resolving the top-level program is not enough. Shebang scripts re-resolve
/// their interpreter through `env` against the CHILD's PATH, and tools such as
/// `npm` (`#!/usr/bin/env node`), `composer` (`php`) and `bundler-audit`
/// (`ruby`) live exactly where a GUI-minimal PATH cannot see. Without this,
/// resolving `/opt/homebrew/bin/npm` succeeds and the spawn still dies with
/// "env: node: No such file or directory" (exit 127) — which reads downstream
/// as "npm is not installed".
///
/// Returns `None` when the joined value cannot be built (an entry with a
/// disallowed character) or when the inherited value already sits at
/// [`MAX_INHERITED_PATH_BYTES`]; the caller then leaves the inherited PATH
/// untouched rather than degrading the child to an empty one — or to one the
/// kernel refuses to exec.
pub(crate) fn extended_child_path(
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<std::ffi::OsString> {
    if path_var.is_some_and(|value| value.len() >= MAX_INHERITED_PATH_BYTES) {
        return None;
    }
    // Only absolute entries survive. An empty entry is POSIX for "the current
    // directory", and `.`, `bin` or `./node_modules/.bin` resolve against the
    // child's cwd, which for git helpers and sidecars is the repository the
    // user opened. Keeping any of them would let an untrusted clone put its
    // own `git-*`, `node` or `npm` ahead of the real ones.
    let mut entries: Vec<PathBuf> = path_var
        .map(std::env::split_paths)
        .into_iter()
        .flatten()
        .filter(|entry| entry.is_absolute())
        .collect();
    if path_var.is_none() && cfg!(unix) {
        // PATH entirely absent (daemon/launchd-style contexts): seed the Unix
        // default search set (confstr `_CS_PATH`) so appending fallbacks does
        // not leave children with nothing but Homebrew dirs.
        entries.push(PathBuf::from("/usr/bin"));
        entries.push(PathBuf::from("/bin"));
    }
    for dir in gui_launch_fallback_dirs(home) {
        if !entries.contains(&dir) {
            entries.push(dir);
        }
    }
    std::env::join_paths(entries).ok()
}

/// Give a hand-built child the same `PATH` [`build_capture_command`] gives its
/// own, unless the caller already chose one.
///
/// Resolving a tool and *running* it are two different PATH questions, and
/// until this existed only the first was answered. [`find_external_tool`]
/// searches the [`gui_launch_fallback_dirs`] on top of `PATH`, so a
/// GUI-launched GitPulse finds `~/.local/bin/devmap` and then spawns it with
/// launchd's `/usr/bin:/bin:/usr/sbin:/sbin` — a PATH that cannot see the
/// binary we just resolved out of it.
///
/// That is not a cosmetic difference for a tool whose job is to inspect the
/// machine. `devmap doctor` resolves the bare `devmap` command named by host
/// MCP configs against its own `PATH`; handed the minimal one it finds
/// nothing, records `exists: false`, and reports "host MCP config names a
/// devmap path that is not a file: devmap — this is not version skew".
/// GitPulse then renders that as an installation fault. It is not one: it is
/// the PATH we chose, described back to us as the user's broken install — a
/// check that could not run, naming a cause it did not have.
///
/// Two conditions keep this from changing anything else:
///
/// - **The caller wins.** A `PATH` already set on the command (the terminal's
///   own environment, [`build_capture_command`], the harness sidecar) is left
///   exactly as it is; this only fills an absent one.
/// - **The program must already be a path.** Rust falls back from
///   `posix_spawn` to `fork` when `PATH` is overridden for a *bare* program
///   name (`env_saw_path() && !program_is_path()` in `std`), and forking a
///   large GUI process is both slow and the shape behind the concurrent-spawn
///   descriptor races this module already guards. Every production spawn that
///   reaches here carries a resolved path; bare names arrive through
///   [`build_capture_command`] or [`git_command_with_env`], which resolve the
///   program *and* set `PATH` themselves, so the first condition already
///   excludes them.
///
/// Non-Windows only, for the reason [`build_capture_command`] gives: the
/// Windows environment block is case-insensitive, so setting `"PATH"` through
/// `.env()` can collide with an inherited `"Path"` and leave the child holding
/// two different search paths.
fn default_child_path(cmd: &mut Command) {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    default_child_path_with_env(cmd, path_var.as_deref(), home.as_deref());
}

/// [`default_child_path`] with the environment passed in, mirroring the
/// [`git_command`] / [`git_command_with_env`] split so the three guards can be
/// tested without writing to the process environment out from under every
/// other test in the binary.
fn default_child_path_with_env(
    cmd: &mut Command,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) {
    if cfg!(windows) {
        return;
    }
    // `components()` rather than a `/` scan: it is the same question `std`
    // asks (`program_is_path`), and it answers it identically for "./devmap"
    // and for a `Path`-typed program built without a literal separator.
    if Path::new(cmd.get_program()).components().count() <= 1 {
        return;
    }
    if cmd
        .get_envs()
        .any(|(key, _)| key == std::ffi::OsStr::new("PATH"))
    {
        return;
    }
    if let Some(child_path) = extended_child_path(path_var, home) {
        cmd.env("PATH", child_path);
    }
}

/// Resolves the `program` argument of [`capture_command`] to a spawner-ready
/// form.
///
/// A name containing a path separator is honored verbatim. A bare name is
/// searched in `path_var` first (preserving normal PATH precedence), then in
/// the [`gui_launch_fallback_dirs`]. Found anywhere, its path is returned;
/// found nowhere, the bare name passes through unchanged so the existing
/// "Failed to spawn …" error keeps naming the tool the caller asked for.
///
/// Crate-visible so the dependency scanner can quote the resolved location of
/// a tool that exists but fails to run, under the same injectable seams its
/// spawns use.
pub(crate) fn resolve_spawn_program_with(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> String {
    resolve_spawn_program_in(program, path_var, home, PathSource::Inherited)
}

/// [`resolve_spawn_program_with`] for a `path_var` of known [`PathSource`]:
/// an explicit one is searched alone.
pub(crate) fn resolve_spawn_program_in(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> String {
    if program.contains('/') || program.contains('\\') {
        return program.to_string();
    }
    find_in_dirs(program, &tool_search_dirs(path_var, home, source))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| program.to_string())
}

/// The non-`PATH` directories [`find_external_tool`] searches, for messages
/// that have to tell a user where a missing tool was looked for.
///
/// Derived from the list the lookup itself uses rather than spelled out at the
/// message site: a hand-written list silently goes stale the moment a
/// directory is added here, and a "we looked in X" that omits where we
/// actually looked is a wrong answer, not a terse one.
pub(crate) fn external_tool_fallback_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    gui_launch_fallback_dirs(home.as_deref())
}

/// Shared lookup for subsystems that need to *find* an external tool without
/// spawning it (the harness sidecar's `manvi`, presence checks that want the
/// resolved path). Single owner of PATH + GUI-fallback resolution semantics;
/// bare names only — anything with a separator is not searched.
pub(crate) fn find_external_tool(program: &str) -> Option<String> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    find_external_tool_with(program, path_var.as_deref(), home.as_deref())
}

/// [`find_external_tool`] with the environment passed in, so a caller can
/// reproduce a GUI launch's PATH without writing to the process environment.
pub(crate) fn find_external_tool_with(
    program: &str,
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Option<String> {
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return None;
    }
    find_in_dirs(
        program,
        &tool_search_dirs(path_var, home, PathSource::Inherited),
    )
    .map(|p| p.to_string_lossy().into_owned())
}

/// Windows' legacy path limit. Beyond it a path needs the `\\?\` prefix to be
/// addressable at all, so a result that long keeps the prefix rather than
/// becoming a spelling Windows cannot open.
#[cfg(windows)]
const WINDOWS_LEGACY_PATH_LIMIT: usize = 260;

/// A `\\?\`-prefixed Windows path rewritten in ordinary form, or `None` when
/// the prefix has to stay.
///
/// Split out as string logic so the rules can be tested on any host: the
/// `Prefix` parse this mirrors only happens on Windows, and these are exactly
/// the cases that were wrong in production.
///
/// Compiled off Windows only for those tests: production has no use for it
/// there, and an ungated copy would be dead code under `-D warnings`.
#[cfg(any(windows, test))]
fn simplified_windows_path(path: &str) -> Option<String> {
    let rest = path.strip_prefix(r"\\?\")?;
    let simplified = if let Some(unc) = rest.strip_prefix(r"UNC\") {
        // \\?\UNC\server\share -> \\server\share
        format!(r"\\{unc}")
    } else {
        // \\?\C:\dir -> C:\dir. Anything that is not a drive-letter root
        // (a device path such as \\?\Volume{...}) has no ordinary spelling.
        let bytes = rest.as_bytes();
        let is_drive_root = bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && bytes.get(1) == Some(&b':')
            && bytes.get(2) == Some(&b'\\');
        if !is_drive_root {
            return None;
        }
        rest.to_string()
    };
    #[cfg(windows)]
    if simplified.len() > WINDOWS_LEGACY_PATH_LIMIT {
        return None;
    }
    Some(simplified)
}

/// `std::fs::canonicalize` in a spelling external tools accept.
///
/// On Windows `canonicalize` always answers with a verbatim `\\?\` path -- it
/// asks the OS for the final name by handle -- and git refuses to use one as a
/// working tree:
///
///   fatal: could not create work tree dir '\\?\C:\...\healthy': Invalid argument
///
/// The prefix also leaks into user-facing text ("Already cloned at \\?\C:\..."),
/// so every caller that hands a resolved path to a child process or to a
/// message wants this rather than `canonicalize`.
///
/// SECURITY: this does not weaken any containment check built on it. The
/// symlink resolution has already happened inside `canonicalize`; removing the
/// prefix does not change which file the path denotes, and callers that
/// compare a child against a parent still compare two paths produced the same
/// way. What it gives up is the extended-length namespace, which is why a
/// result that no longer fits the legacy limit keeps its prefix instead of
/// becoming a path Windows would refuse to open.
///
/// On every other platform this is `canonicalize`, unchanged.
pub(crate) fn canonicalize_plain(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = path.canonicalize()?;
    Ok(match canonical.to_str() {
        Some(text) => PathBuf::from(plain_path_text(text)),
        None => canonical,
    })
}

/// `path` in the spelling git takes as a command-line argument.
///
/// [`canonicalize_plain`] for a path that is already in hand as text: the
/// repository admission and trust identity spell every checkout `\\?\C:\...`
/// (the sandbox containment checks depend on that), and git refuses that form
/// as an argument -- `git worktree add '\\?\C:\...\wt'` dies with `could not
/// create leading directories ... Invalid argument`. A path with no ordinary
/// spelling (beyond the legacy limit, a device path) is returned unchanged.
///
/// The same string reaches the command gate and the spawn, so both judge one
/// spelling. Identity off Windows, where a backslash is a filename character.
pub(crate) fn plain_path_text(path: &str) -> String {
    #[cfg(windows)]
    if let Some(simplified) = simplified_windows_path(path) {
        return simplified;
    }
    path.to_owned()
}

/// Assembles the `capture_command` child with injectable `PATH`/home lookups,
/// mirroring the seam [`resolve_spawn_program_with`] gives the resolver.
///
/// Bare names are resolved up front: the child inherits our environment, but
/// `Command` performs its PATH walk with it, so a GUI-minimal PATH cannot be
/// patched from inside the child. The resolved script's own interpreter lookup
/// (`env node` inside npm) walks the same inherited PATH, so the fallback dirs
/// are appended to the child's PATH too — non-Windows only, where setting
/// "PATH" via `.env()` cannot collide with the case-insensitive "Path"
/// variable in the Windows environment block.
pub(crate) fn build_capture_command(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
) -> Command {
    build_capture_command_in(
        program,
        args,
        cwd,
        extra_env,
        path_var,
        home,
        PathSource::Inherited,
    )
}

/// [`build_capture_command`] for a `path_var` of known [`PathSource`]. An
/// explicit PATH is both the resolution search path and, verbatim apart from
/// dropped empty entries, the child's PATH — so nothing the child re-resolves
/// (a shebang interpreter, a nested tool) reaches past it either.
fn build_capture_command_in(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Command {
    let resolved = resolve_spawn_program_in(program, path_var, home, source);
    let mut cmd = Command::new(&resolved);
    // The same environment [`git_command`] gives git ([`pin_git_environment`]):
    // captured tools shell out to git themselves (`gh pr checkout` fetches and
    // merges in the repo; npm/go/cargo read git config), and CI-style inherited
    // pointers (GIT_DIR, GIT_INDEX_FILE, GIT_CONFIG_PARAMETERS, ...) would
    // redirect them off the repository the caller picked, a localized message
    // would defeat the parsers, and a credential manager could prompt. Applied
    // before `extra_env`, so a caller that explicitly passes one of these
    // still wins.
    pin_git_environment(&mut cmd);
    cmd.env("GH_PROMPT_DISABLED", "1")
        // gh consults this before printing update notices; keep subprocess
        // output deterministic instead of interleaving a self-update banner.
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if !cfg!(windows) {
        // Applied before `extra_env` so an explicit caller-provided PATH wins.
        match source {
            PathSource::Inherited => {
                if let Some(child_path) = extended_child_path(path_var, home) {
                    cmd.env("PATH", child_path);
                }
            }
            // Never left to inherit: the process PATH is exactly what an
            // explicit one replaces.
            PathSource::Explicit => {
                match path_var.and_then(|value| {
                    std::env::join_paths(tool_search_dirs(Some(value), home, source)).ok()
                }) {
                    Some(child_path) => cmd.env("PATH", child_path),
                    None => cmd.env_remove("PATH"),
                };
            }
        }
    }
    for (key, value) in extra_env {
        cmd.env(key, value);
    }
    cmd
}

/// Runs `program` with a hard timeout and bounded pipes.
///
/// `cwd` is optional. When set, it must already be a directory the caller has
/// judged (a validated repo, or a path `sandbox_join` produced). This helper
/// does not re-validate the path. Non-zero exits are returned, not raised.
pub fn capture_command(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
) -> Result<CapturedOutput, CommandError> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    capture_command_with_env(
        program,
        args,
        cwd,
        timeout,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
        PathSource::Inherited,
    )
}

/// Names a scrubbed child may inherit (compared upper-cased). An allowlist, not
/// a denylist: a token variable nobody has heard of yet is dropped by default.
/// Proxy and CA variables are the user's own transport settings, not
/// credentials a repository's config can ask npm to expand into a request.
const SCRUBBED_ENV_ALLOWLIST: &[&str] = &[
    "HOME",
    "USERPROFILE",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "TMPDIR",
    "TMP",
    "TEMP",
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "BUN_INSTALL",
    "VOLTA_HOME",
    "ASDF_DIR",
    "ASDF_DATA_DIR",
    "NVM_DIR",
    "NVM_BIN",
    "FNM_DIR",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "NO_PROXY",
    "ALL_PROXY",
    "NODE_EXTRA_CA_CERTS",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "SYSTEMROOT",
    "SYSTEMDRIVE",
    "WINDIR",
    "COMSPEC",
    "PATHEXT",
    "APPDATA",
    "LOCALAPPDATA",
    "PROGRAMFILES",
    "PROGRAMDATA",
    "HOMEDRIVE",
    "HOMEPATH",
];

fn scrubbed_env_allows(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "PATH" || upper.starts_with("LC_") || SCRUBBED_ENV_ALLOWLIST.contains(&upper.as_str())
}

/// Replaces `cmd`'s environment with the allowlisted part of `inherited` plus
/// everything the builder set explicitly (PATH, the git pins, `extra_env`).
/// A variable the builder explicitly removed stays removed. `also_allowed`
/// names further inherited variables to keep (the ones the user's own npm
/// config references). This form keeps only the fixed allowlist; production
/// goes through [`restrict_env_to_allowlist_plus`].
#[cfg(test)]
fn restrict_env_to_allowlist<I>(cmd: &mut Command, inherited: I, home: Option<&std::ffi::OsStr>)
where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    restrict_env_to_allowlist_plus(cmd, inherited, home, &[]);
}

/// [`restrict_env_to_allowlist`] that also keeps the inherited variables named
/// in `also_allowed`.
fn restrict_env_to_allowlist_plus<I>(
    cmd: &mut Command,
    inherited: I,
    home: Option<&std::ffi::OsStr>,
    also_allowed: &[String],
) where
    I: IntoIterator<Item = (std::ffi::OsString, std::ffi::OsString)>,
{
    let mut explicit = Vec::new();
    let mut removed = Vec::new();
    for (key, value) in cmd.get_envs() {
        match value {
            Some(value) => explicit.push((key.to_os_string(), value.to_os_string())),
            None => removed.push(key.to_os_string()),
        }
    }
    cmd.env_clear();
    for (key, value) in inherited {
        let allowed = key.to_str().is_some_and(|name| {
            scrubbed_env_allows(name) || also_allowed.iter().any(|n| n == name)
        });
        if allowed && !removed.contains(&key) {
            cmd.env(key, value);
        }
    }
    if let Some(home) = home {
        cmd.env(if cfg!(windows) { "USERPROFILE" } else { "HOME" }, home);
    }
    for (key, value) in explicit {
        cmd.env(key, value);
    }
}

/// [`capture_command_with_env`] for a tool that reads repository-controlled
/// config (npm, bun): the child gets an allowlisted environment instead of the
/// process's, so `NPM_TOKEN`, `GITHUB_TOKEN`, `AWS_*` and the like cannot be
/// expanded into a request. Git variables are still pinned as for any capture.
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_command_scrubbed(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
    also_allowed: &[String],
) -> Result<CapturedOutput, CommandError> {
    let mut cmd = build_capture_command_in(program, args, cwd, extra_env, path_var, home, source);
    restrict_env_to_allowlist_plus(&mut cmd, std::env::vars_os(), home, also_allowed);
    let out = run_bounded(cmd, program, timeout, None)?.require_complete(program)?;
    Ok(CapturedOutput {
        stdout: out.stdout,
        stderr: out.stderr,
        success: out.success,
        status_code: out.status_code,
    })
}

/// [`capture_command`] with injectable `PATH`/home lookups — the seam its
/// callers fill from the process environment by default. Tests use it to run
/// the exact production spawn path under a simulated GUI-minimal environment
/// without mutating process-global state. `source` says which of those two
/// `path_var` is; see [`PathSource`].
#[allow(clippy::too_many_arguments)]
pub(crate) fn capture_command_with_env(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    path_var: Option<&std::ffi::OsStr>,
    home: Option<&std::ffi::OsStr>,
    source: PathSource,
) -> Result<CapturedOutput, CommandError> {
    let cmd = build_capture_command_in(program, args, cwd, extra_env, path_var, home, source);

    let out = run_bounded(cmd, program, timeout, None)?.require_complete(program)?;
    Ok(CapturedOutput {
        stdout: out.stdout,
        stderr: out.stderr,
        success: out.success,
        status_code: out.status_code,
    })
}

/// Tail of one byte stream as display text, cut on a char boundary so a
/// multibyte character split at the cap does not render as replacement
/// garbage. Shared by every surface that shows capped tool output.
pub fn byte_tail(bytes: &[u8], cap: usize) -> String {
    let start = bytes.len().saturating_sub(cap);
    let tail = &bytes[start..];
    let boundary = tail
        .iter()
        .position(|b| (*b & 0xC0) != 0x80)
        .unwrap_or(tail.len());
    String::from_utf8_lossy(&tail[boundary..]).into_owned()
}

/// A finished run (any exit code) with capped per-stream tails.
#[derive(Debug)]
pub struct CapturedRun {
    pub stdout_tail: String,
    pub stderr_tail: String,
    pub success: bool,
    pub status_code: i32,
    /// True when the tails are a prefix, for any of the reasons below.
    pub truncated: bool,
    /// Which reason, in one clause. `None` when nothing was cut. Carried
    /// beside the flag because "we stopped reading at the display budget" and
    /// "we never finished reading" are different facts, and a surface that
    /// renders one sentence for both asserts a cause it cannot know.
    pub truncation_reason: Option<String>,
}

/// Outcome of [`run_captured`]. A timeout is an outcome, not an error: the
/// terminal surfaces "killed at N s" next to normal exits, and collapsing it
/// into a spawn-style error string would make the two indistinguishable.
#[derive(Debug)]
pub enum RunOutcome {
    Finished(CapturedRun),
    TimedOut(Duration),
}

/// Like [`capture_command`], but truncation keeps capped tails instead of
/// erroring away everything, and the timeout is reported as an outcome.
///
/// `tail_cap` bounds each returned tail in bytes; pass [`MAX_OUTPUT_BYTES`]
/// to keep everything up to the drain cap. Built for callers that show raw
/// tool output to a user (the terminal); `capture_command` stays the right
/// shape for JSON-scraping scanners.
pub fn run_captured(
    program: &str,
    args: &[&str],
    cwd: Option<&Path>,
    timeout: Duration,
    extra_env: &[(&str, &str)],
    tail_cap: usize,
) -> Result<RunOutcome, CommandError> {
    let path_var = std::env::var_os("PATH");
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"));
    let cmd = build_capture_command(
        program,
        args,
        cwd,
        extra_env,
        path_var.as_deref(),
        home.as_deref(),
    );
    // Spawn/wait failures stay errors. The timeout is detected from the error
    // `run_bounded` formats for exactly that case (see TIMEOUT_MARKER); the
    // regression test below pins this contract so a rewording cannot silently
    // turn timeouts into spawn errors again.
    match run_bounded(cmd, program, timeout, None) {
        Ok(out) => {
            // The drain's own reason wins: it describes the whole stream. The
            // display cap only says this VIEW is a tail, which is a weaker and
            // separate claim.
            let cut_for_display = out.stdout.len() > tail_cap || out.stderr.len() > tail_cap;
            let truncation_reason =
                out.incomplete
                    .as_ref()
                    .map(Incomplete::describe)
                    .or_else(|| {
                        cut_for_display.then(|| format!("only the last {tail_cap} bytes are shown"))
                    });
            Ok(RunOutcome::Finished(CapturedRun {
                stdout_tail: byte_tail(&out.stdout, tail_cap),
                stderr_tail: byte_tail(&out.stderr, tail_cap),
                success: out.success,
                status_code: out.status_code,
                truncated: truncation_reason.is_some(),
                truncation_reason,
            }))
        }
        Err(e) if is_timeout_error(program, e.message()) => Ok(RunOutcome::TimedOut(timeout)),
        Err(e) => Err(e),
    }
}

/// The exact phrase [`run_bounded`] embeds when a deadline kills a child and
/// the only marker [`run_captured`] matches to classify a timeout. Formatter
/// and matcher both go through this constant, so the two sides of the
/// stringly-typed seam cannot drift apart silently; the regression test
/// additionally pins the whole behavior end to end.
const TIMEOUT_MARKER: &str = " timed out after ";

/// The phrase a [`Refusal::Refused`] or [`Refusal::Shed`] error carries, and
/// the only thing [`is_deferred_under_load`] matches. A deferral is the app
/// declining to start more work under load, not git failing: it must never
/// carry [`TIMEOUT_MARKER`], or every reader of the error calls it a hang.
const DEFERRED_MARKER: &str = " deferred under load after ";

/// The git command `args` runs, past any global options before it, for the
/// labels that name a spawn in the gate's report and in errors. The first
/// argument named `-c` for every read that set a config value, and an
/// `--attr-source=<oid>` would have put an object id where a command belongs.
pub(crate) fn subcommand<'a>(args: &[&'a str]) -> &'a str {
    subcommand_index(args).map_or("", |at| args[at])
}

/// Where in `args` the subcommand is, past git's global options.
fn subcommand_index(args: &[&str]) -> Option<usize> {
    /// Global options whose value is the next argument.
    const TAKES_VALUE: [&str; 6] = [
        "-c",
        "-C",
        "--git-dir",
        "--work-tree",
        "--namespace",
        "--config-env",
    ];
    let mut rest = args.iter().enumerate();
    while let Some((at, arg)) = rest.next() {
        if TAKES_VALUE.contains(arg) {
            rest.next();
        } else if !arg.starts_with('-') {
            return Some(at);
        }
    }
    None
}

/// `err` without the one optional `[code] ` prefix the ledger adds. Every
/// anchored check on a runner message goes through this, so none can be
/// defeated by the prefix the others tolerate.
fn strip_code_prefix(err: &str) -> &str {
    match err.strip_prefix('[').and_then(|rest| rest.split_once("] ")) {
        Some((code, rest)) if code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => rest,
        _ => err,
    }
}

/// True when `err` is the gate declining to start a child under load. The
/// command may succeed if asked again once load falls; it did not run.
///
/// Anchored to the one shape [`refusal_message`] writes, so text that merely
/// quotes the phrase (a path inside "Not a Git repository: <path>", a worktree
/// named in "does not belong") is not a deferral. The shape is, after an
/// optional `[code] ` prefix the ledger adds: a label of plain words (the
/// program and subcommand, never a path or a quote), [`DEFERRED_MARKER`], then
/// the wait as `<seconds>s:`. The label ends at the FIRST marker, so a hostile
/// path that carries a complete refusal later in the text cannot qualify.
pub(crate) fn is_deferred_under_load(err: &str) -> bool {
    let body = strip_code_prefix(err);
    let Some((label, after)) = body.split_once(DEFERRED_MARKER) else {
        return false;
    };
    let plain_label = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    let waited = after.split_once("s:").is_some_and(|(seconds, _)| {
        let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
        [whole, fraction]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    plain_label && waited
}

/// True when `err` is a child that never got a concurrency slot inside its
/// own deadline — as opposed to one that started and then ran out of time.
pub(crate) fn is_slot_wait_timeout(err: &str) -> bool {
    err.contains(TIMEOUT_MARKER) && err.contains(SLOT_WAIT_SUFFIX)
}

const SLOT_WAIT_SUFFIX: &str = "s waiting for a process slot";

/// The phrase a spawn error carries when the OS found nothing to run — the
/// only failure that answers "is this tool installed?" with no. Every other
/// spawn failure (too many open files, an argument list too long, a busy
/// fork, a refused trust check) says the system could not start a program,
/// which is not evidence the program is missing.
///
/// With a working directory set, a missing directory reports the same OS
/// error, so a presence probe passes none.
const SPAWN_NOT_FOUND_MARKER: &str = ": no such program: ";

/// The error text for a failed `spawn`, formatted in one place so
/// [`is_spawn_not_found`] can only match what this wrote.
fn spawn_error(label: &str, error: &std::io::Error) -> String {
    if error.kind() == std::io::ErrorKind::NotFound {
        format!("Failed to spawn {label}{SPAWN_NOT_FOUND_MARKER}{error}")
    } else {
        format!("Failed to spawn {label}: {error}")
    }
}

/// True when `err` is a spawn the OS refused because nothing exists at that
/// program name.
pub(crate) fn is_spawn_not_found(err: &str) -> bool {
    err.starts_with("Failed to spawn ") && err.contains(SPAWN_NOT_FOUND_MARKER)
}

/// What a `tool --version` run says about the tool.
///
/// Four answers, because "no" has to be earned. Only the OS failing to find
/// the program is [`ToolProbe::NotFound`]; a run that started and went wrong
/// is [`ToolProbe::FoundButFailed`]; a run that never started — deferred
/// under load, no process slot in time, a spawn the system refused — is
/// [`ToolProbe::NotRun`], which says nothing about whether the tool exists.
/// Reporting either of the last two as "not installed" names a cause nobody
/// observed: GitPulse told a user with `gh` on PATH to install it, because
/// its probe had been shed while the machine was busy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ToolProbe {
    /// Exited 0 and printed a non-empty first stdout line.
    Present(String),
    /// The OS found nothing at that name; carries the spawn error text.
    NotFound(String),
    /// Found and started, but no version came back: non-zero exit (with its
    /// stderr), a timeout after start, empty output. Carries the diagnosis.
    FoundButFailed(String),
    /// Never started, for a reason other than absence. Carries the reason.
    NotRun(String),
}

impl ToolProbe {
    pub(crate) fn version(&self) -> Option<&str> {
        match self {
            ToolProbe::Present(version) => Some(version),
            _ => None,
        }
    }
}

/// Classifies one probe run. `result` is what [`capture_command`] (or a
/// seam over it) returned for `program`.
pub(crate) fn classify_tool_probe(
    program: &str,
    result: Result<CapturedOutput, CommandError>,
) -> ToolProbe {
    // A probe's answer is a status the caller shows, not a command's error, so
    // the text is all it keeps.
    match result.map_err(CommandError::into_message) {
        Ok(out) => {
            let stdout = out.stdout_text();
            let line = stdout.lines().next().unwrap_or("").trim();
            if out.success && !line.is_empty() {
                ToolProbe::Present(line.to_string())
            } else {
                ToolProbe::FoundButFailed(found_but_failed_detail(program, &out))
            }
        }
        Err(error) if is_spawn_not_found(&error) => ToolProbe::NotFound(error),
        Err(error)
            if is_deferred_under_load(&error)
                || is_slot_wait_timeout(&error)
                || error.starts_with("Failed to spawn ")
                || error.contains("cancelled before spawn") =>
        {
            ToolProbe::NotRun(error)
        }
        // A timeout after start, an output cap, a failed wait: the program
        // was there and the run went wrong.
        Err(error) => ToolProbe::FoundButFailed(error),
    }
}

/// Diagnosis for a finished run that yielded no usable version line.
///
/// Success is checked before stdout so a non-zero exit can never be promoted
/// to "present" by incidental output; a mute stderr falls back to naming the
/// exit status rather than leaving an unexplained failure.
pub(crate) fn found_but_failed_detail(program: &str, out: &CapturedOutput) -> String {
    if out.success {
        return format!("{program} produced no version output");
    }
    let err = out.stderr_text();
    if err.is_empty() {
        format!("{program} exited {} without a diagnosis", out.status_code)
    } else {
        err
    }
}

/// Which of the gate's two rate buckets refused a spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusingBucket {
    /// This process's own bucket (the process gate).
    Local,
    /// The cross-process record every GitPulse process of the user draws from.
    Shared,
}

/// What a refusing bucket held when it refused: it admits a spawn only while
/// more than `floor` tokens remain (`0` for reactive work, the reserve kept
/// for reactive reads for background work), and refills up to `burst`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BucketLevel {
    pub bucket: RefusingBucket,
    pub tokens: u32,
    pub floor: u32,
    pub burst: u32,
}

impl BucketLevel {
    /// `shared bucket: 0 tokens, needs more than 48, burst 192`.
    fn describe(&self) -> String {
        let bucket = match self.bucket {
            RefusingBucket::Local => "local",
            RefusingBucket::Shared => "shared",
        };
        format!(
            "{bucket} bucket: {} token(s), needs more than {}, burst {}",
            self.tokens, self.floor, self.burst
        )
    }
}

#[cfg(test)]
pub(crate) fn refusal_message(label: &str, refusal: Refusal) -> CommandError {
    refusal_message_at(label, refusal, None)
}

/// [`refusal_message`], naming the bucket that refused and its level when the
/// gate knows them. The text before the level is unchanged, so every reader
/// that matches on the deferral (see [`is_deferred_under_load`]) reads it as
/// before; the level is a parenthesised tail after the sentence.
pub(crate) fn refusal_message_at(
    label: &str,
    refusal: Refusal,
    level: Option<BucketLevel>,
) -> CommandError {
    let by = level.map_or_else(String::new, |level| format!(" ({})", level.describe()));
    // A deferral is typed where it is written (see `crate::ipc_error`): the
    // error carries the kind to the command's edge, and nothing reads the
    // marker back out of a message.
    let deferred =
        |text: String| CommandError::typed(crate::ipc_error::IpcErrorKind::DeferredUnderLoad, text);
    match refusal {
        Refusal::Cancelled => format!("{label} cancelled before spawn").into(),
        Refusal::TimedOut { deadline } => format!(
            "{label}{TIMEOUT_MARKER}{:.3}{SLOT_WAIT_SUFFIX}",
            deadline.as_secs_f64()
        )
        .into(),
        Refusal::Refused { waited } => deferred(format!(
            "{label}{DEFERRED_MARKER}{:.3}s: the git spawn rate limit admitted nothing sooner{by}",
            waited.as_secs_f64()
        )),
        Refusal::Shed => deferred(format!(
            "{label}{DEFERRED_MARKER}0.000s: background work is shed while the git spawn rate \
             budget is low{by}"
        )),
        Refusal::Pressure { reactive } => deferred(format!(
            "{label}{DEFERRED_MARKER}0.000s: {} work is shed while the host is under memory \
             pressure",
            if reactive { "reactive" } else { "background" }
        )),
    }
}

/// First sleep between `try_wait` polls, and the ceiling it grows to.
///
/// A fixed 15 ms sleep quantized EVERY git invocation upward by up to a full
/// interval: a `git rev-parse` that exits in 2 ms was not observed for another
/// 13, and GitPulse spends most of its subprocess budget on exactly these
/// short reads — `git status` measures ~20 ms on a mid-size repository and
/// `for-each-ref` ~10 ms, so the sleep was 75-150% overhead on the common
/// case. It compounds through sequential chains: `list_branches` alone spawns
/// four processes before it returns a row.
///
/// Starting fine and backing off keeps the fast case fast without spinning on
/// a slow one. A `git clone` that runs for a minute reaches the ceiling after
/// ~13 polls and then behaves exactly as before; the polls spent getting there
/// cost well under a millisecond of CPU in total.
const POLL_BACKOFF_START: Duration = Duration::from_micros(250);
const POLL_BACKOFF_MAX: Duration = Duration::from_millis(15);

/// Next backoff step: double, capped. Pure so the schedule is testable without
/// spawning a process.
fn next_poll_backoff(current: Duration) -> Duration {
    current.saturating_mul(2).min(POLL_BACKOFF_MAX)
}

/// True when `err` says a command ended because its own timeout elapsed,
/// whether it ran out of time while running or while waiting for a process
/// slot. A load deferral never carries the marker, so it is not one.
pub(crate) fn is_timed_out(err: &str) -> bool {
    err.contains(TIMEOUT_MARKER)
}

fn is_timeout_error(program: &str, err: &str) -> bool {
    err.starts_with(program) && is_timed_out(err)
}

/// What a runner failure message says happened, read once and in one place.
///
/// The runner still hands callers the `String` it always did (hooks, MCP and
/// tests match on its text, so the text is unchanged); this is the typed view
/// every classifier outside this module reads instead of matching substrings of
/// its own. Order of precedence is the order of the variants that can overlap:
/// a deferral and a slot-wait refusal both read like a timeout to a bare
/// substring match, and neither one ran the child.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunError {
    /// The gate declined to start the child under load; asking again later may
    /// succeed. See [`is_deferred_under_load`].
    Deferred,
    /// The child never got a concurrency slot inside its own deadline. It did
    /// not start, so it did not run out of time. See [`is_slot_wait_timeout`].
    SlotWaitTimeout,
    /// The child started and ran past its own deadline.
    Timeout,
    /// The OS found nothing to run at the program name.
    SpawnNotFound,
    /// The OS refused to start the program for any other reason.
    SpawnFailed,
    /// Anything else: the child ran and failed, or the text is not the
    /// runner's.
    Other,
}

impl RunError {
    /// Classify a runner failure message. Spawn failures are recognised before
    /// timeouts so a program path that spells the timeout phrase cannot turn a
    /// spawn failure into a hang.
    pub(crate) fn classify(err: &str) -> Self {
        let body = strip_code_prefix(err);
        if is_deferred_under_load(err) {
            Self::Deferred
        } else if is_spawn_not_found(body) {
            Self::SpawnNotFound
        } else if body.starts_with("Failed to spawn ") {
            Self::SpawnFailed
        } else if is_slot_wait_timeout(err) {
            Self::SlotWaitTimeout
        } else if is_runner_timeout_shape(body) {
            Self::Timeout
        } else {
            Self::Other
        }
    }
}

/// The runner's own deadline text, `<label> timed out after <n>s`, anchored at
/// the start: a label of plain words (program and subcommand, or a program
/// path), the marker, then the seconds. Text a child printed and a caller
/// relayed (`... Operation timed out after 30001 milliseconds`) has a label
/// full of punctuation and a unit that is not `s`, and is not this.
fn is_runner_timeout_shape(body: &str) -> bool {
    let Some((label, after)) = body.split_once(TIMEOUT_MARKER) else {
        return false;
    };
    let plain_label = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.' | '/'));
    let digits = after
        .bytes()
        .take_while(|b| b.is_ascii_digit() || *b == b'.')
        .count();
    plain_label && digits > 0 && after[digits..].starts_with('s')
}

/// True when `err` is a child that started and ran past its deadline: not a
/// deferral, not a refused slot wait, not a spawn failure.
pub(crate) fn is_run_timeout(err: &str) -> bool {
    RunError::classify(err) == RunError::Timeout
}

/// What one pipe drain produced.
///
/// `stop` is the field that keeps a broken read distinguishable from a short
/// one: `bytes` may be a perfectly well-formed prefix either way, and only
/// this says whether the rest is missing because there was no more or because
/// we stopped being able to read it.
#[derive(Default)]
struct Drained {
    bytes: Vec<u8>,
    /// Bytes already handed to a streaming observer and dropped from `bytes`
    /// (see [`ProcessObserver::drains_stdout`]); they still count against the
    /// cap, so a streamed read is as bounded as a buffered one.
    discarded: usize,
    /// Cut off at the byte cap: the child had more to say.
    truncated: bool,
    /// Why the read stopped, when it was not end-of-stream.
    stop: Option<Stop>,
}

impl Drained {
    fn append(&mut self, bytes: &[u8], cap: usize) {
        let held = self.bytes.len().saturating_add(self.discarded);
        let take = bytes.len().min(cap.saturating_sub(held));
        self.bytes.extend_from_slice(&bytes[..take]);
        self.truncated |= take < bytes.len();
    }

    /// Drops what an observer has now seen, keeping its length in the budget.
    fn discard_observed(&mut self) {
        self.discarded = self.discarded.saturating_add(self.bytes.len());
        self.bytes.clear();
    }
}

/// The two ways a drain ends without reaching end-of-stream. They are kept
/// apart because they deserve different answers: one is a fault, the other is
/// a known-incomplete read of a child that did exit cleanly.
enum Stop {
    /// The read itself failed. What arrived is a fragment of unknown length,
    /// and nothing downstream can tell it from a complete short output.
    Broken(String),
    /// EOF was not observed inside the grace window, or the blocking fallback
    /// reader did not deliver. The child's own status is still trustworthy.
    Undelivered(String),
}

/// Why captured stdout is a prefix of what the child actually wrote.
///
/// This exists because a single `truncated: bool` was asked to mean two
/// unrelated things — "the child said more than the budget allowed" and "we
/// never finished reading it" — and the callers that turn the flag into a
/// sentence could only name one of them. They named the wrong one: a
/// `for-each-ref` whose entire output was 1,482 bytes was reported to the user
/// as `output exceeded 64 MB`, because the drain thread missed its delivery
/// window on a loaded machine. A check that could not run must not report the
/// same way as one that ran and found too much.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Incomplete {
    /// stdout reached the caller's byte budget; the rest was dropped. The
    /// amount missing is unknown but the child was read to the end of the cap.
    OverCap(usize),
    /// The read did not finish inside the grace window, so how much is missing
    /// — if anything — is unknown. Captured bytes may still be available.
    Unread(String),
    /// The child was stopped because its runtime deadline elapsed. Captured
    /// bytes are a prefix of what it would have written. `over_cap` is set
    /// when that prefix had already filled the caller's byte budget.
    ///
    /// `message` is the same sentence a timeout error carries
    /// (`"{label} timed out after {n}s"`), so a caller that cannot accept a
    /// prefix reports the deadline and not a slot wait or a deferral.
    Deadline {
        message: String,
        over_cap: Option<usize>,
    },
}

impl Incomplete {
    /// The clause a caller appends after naming its subject — "git blame
    /// output", "This diff", "Log output". Deliberately subject-free so one
    /// renderer serves an error string, an AI warning and two UI banners
    /// without any of them re-deriving the cause and getting it wrong.
    ///
    /// Every arm says what happened; none invents a cause.
    pub fn describe(&self) -> String {
        match self {
            // Whole MiB when the budget is one, exact bytes otherwise: a
            // 512 KiB cap rendered as "exceeded 0 MB" reads as a bug in the
            // message rather than a fact about the output.
            Incomplete::OverCap(cap) => over_cap_phrase(*cap),
            Incomplete::Unread(why) => format!("could not be read to the end ({why})"),
            // The byte budget is the proven bound when both happened. The
            // deadline message stays on the variant for callers that still
            // have to fail a partial stream.
            Incomplete::Deadline {
                over_cap: Some(cap),
                ..
            } => over_cap_phrase(*cap),
            Incomplete::Deadline {
                message,
                over_cap: None,
            } => format!("stopped at its deadline ({message})"),
        }
    }
}

fn over_cap_phrase(cap: usize) -> String {
    if cap >= 1024 * 1024 {
        format!("exceeded {} MB", cap / (1024 * 1024))
    } else {
        format!("exceeded {cap} bytes")
    }
}

/// What [`run_bounded`] observed, before a caller shapes its own errors.
#[derive(Clone, Debug)]
pub(crate) struct BoundedRun {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub success: bool,
    pub status_code: i32,
    /// Why `stdout` is a prefix, when it is; `None` when it is the whole
    /// stream. Carried as a reason rather than a flag so no caller has to
    /// guess which of the two causes it is looking at.
    pub incomplete: Option<Incomplete>,
    pub stderr_incomplete: Option<Incomplete>,
    pub cancelled: bool,
}

impl BoundedRun {
    /// Parsers must establish completeness before interpreting a prefix.
    pub(crate) fn require_complete(self, label: &str) -> Result<Self, CommandError> {
        for (stream, reason) in [
            ("output", &self.incomplete),
            ("stderr", &self.stderr_incomplete),
        ] {
            if let Some(reason) = reason {
                return Err(format!("{label} {stream} {}", reason.describe()).into());
            }
        }
        Ok(self)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum OutputStream {
    Stdout,
    Stderr,
}

/// Callbacks run on the waiter, must return promptly, and receive only the
/// bounded captured prefix. They never run on a pipe worker.
pub(crate) trait ProcessObserver {
    fn cancelled(&self) -> bool {
        false
    }
    fn output(&mut self, _stream: OutputStream, _bytes: &[u8]) {}
    /// True when the observer keeps what it needs from stdout itself, so the
    /// run need not retain it: each chunk is dropped once `output` has seen it
    /// and the returned [`BoundedRun::stdout`] holds only the unseen tail
    /// (empty in practice). The byte cap still applies to the stream as a
    /// whole. For a reader of a long stream that wants a few bytes per record
    /// out of megabytes, which would otherwise be buffered whole.
    fn drains_stdout(&self) -> bool {
        false
    }
}

impl ProcessObserver for () {}

fn observe_output(
    observer: &mut dyn ProcessObserver,
    cursors: &mut [usize; 2],
    stdout: &[u8],
    stderr: &[u8],
) {
    for (index, (stream, bytes)) in [
        (OutputStream::Stdout, stdout),
        (OutputStream::Stderr, stderr),
    ]
    .into_iter()
    .enumerate()
    {
        if bytes.len() > cursors[index] {
            observer.output(stream, &bytes[cursors[index]..]);
            cursors[index] = bytes.len();
        }
    }
}

/// Ceiling on how many child processes this engine keeps alive at once.
///
/// Every `git`, `gh` and `npm` invocation in the app funnels through
/// [`run_bounded`], and each live one costs the parent two pipe descriptors,
/// up to two fallback drain threads off Unix, and up to
/// `MAX_OUTPUT_BYTES + 4 MiB` of buffered output.
/// Nothing bounded how many could be in flight at once, and three layers above
/// this one are happy to ask for hundreds: Tauri's blocking pool admits 512
/// concurrent tasks (tokio's default `max_blocking_threads`), `off_thread`
/// hands every command straight to it, and a workspace-wide refresh fans out
/// over as many as `MAX_BULK_TARGETS` repositories with five commands each.
///
/// The host says no long before the app does. A GUI launch inherits launchd's
/// soft `RLIMIT_NOFILE` of 256, so a few dozen simultaneous children exhaust
/// the descriptor table and *every* subsequent spawn fails with EMFILE —
/// surfacing as the "Failed to spawn git ...: Too many open files (os error
/// 24)" storm, which does not clear on its own because the UI retries into the
/// same wall. [`crate::limits::raise_open_file_limit`] buys the headroom back;
/// this gate is what stops the app from spending it all at once.
///
/// The permit spans the child's whole life, not just the spawn call, because
/// that is how long its descriptors, threads and buffers are held.
const SPAWN_LIMIT_FLOOR: usize = 4;
const SPAWN_LIMIT_CEILING: usize = 16;

/// Who is asking for a child, which decides what the gate may refuse it.
///
/// The measured storm on 2026-10-02 was a watcher-triggered refresh, and at
/// launch the rate cap that stopped it also refused the user's own actions,
/// because the gate could not tell the two apart. The class is the answer to
/// "what may be refused": a refresh can be deferred, a click must not be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Admission {
    /// A user action: a mutation behind the command guard. Concurrency-limited
    /// only. It never waits for, and never spends, a rate token, so no amount
    /// of refresh traffic can refuse the action that caused it.
    Interactive,
    /// The default for every command and thread: the reads a `repo-changed`
    /// event, a poll or a tab switch asks for. Concurrency- and rate-limited,
    /// and identical queued reads share one child (see [`run_read_shared`]).
    Reactive,
    /// Indexing, docs and live-index work. At most a quarter of the slots, and
    /// shed — refused without waiting — while the rate budget is at or below
    /// the reserve kept for reactive reads.
    Background,
}

impl Admission {
    const ALL: [Admission; 3] = [Self::Interactive, Self::Reactive, Self::Background];

    fn index(self) -> usize {
        match self {
            Self::Interactive => 0,
            Self::Reactive => 1,
            Self::Background => 2,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Reactive => "reactive",
            Self::Background => "background",
        }
    }
}

/// Whether every thread of this process starts in [`Admission::Background`].
///
/// The app leaves it unset. `gitpulse-mcp` and `gitpulsed` set it once, first
/// thing in `main`: their whole job is optional work on behalf of an agent or
/// a schedule, so under load it should be shed before the app the user is
/// looking at is deferred. A process-wide default rather than a scope, because
/// a scope ends at a thread hop — a request worker, or rayon's pool behind a
/// `par_iter` — and every hop would otherwise be promoted to `Reactive`.
static PROCESS_BACKGROUND: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Makes [`Admission::Background`] the class every thread of this process
/// starts in. Call before any thread is started; a thread that has already
/// asked keeps the class it was given.
pub fn run_process_as_background() {
    PROCESS_BACKGROUND.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether background spawns in this process wait for the rate budget instead
/// of being shed (see [`run_process_awaiting_background_budget`]).
static PROCESS_BACKGROUND_AWAITS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Background spawns of this process wait, for at most [`SPAWN_QUEUE_BUDGET`],
/// for the budget to climb back above the background reserve, rather than
/// being shed at once. Call before any thread is started, like
/// [`run_process_as_background`].
///
/// Admission class of an agent-initiated MCP read: **Background, waiting**.
/// It is not Reactive, because the reserve exists so that the edit-time hook
/// (Reactive, gating the user's own tool call) and the app's reads keep budget
/// while agent traffic is heavy; fifteen agent sessions each taking an
/// insights snapshot (about 20 spawns on 18 worktrees, measured) would
/// otherwise spend the shared record to zero and get hooks refused. It is not
/// plain Background, because that sheds at 0.000s whenever the shared or local
/// budget sits at or below the reserve, and a caller blocked on the answer then
/// gets a refusal for a condition that clears in a fraction of a second at 8
/// tokens a second. Waiting keeps the floor (this class never spends the
/// reserve, so the shared budget still bounds it exactly as before) and turns
/// "shed while low" into "paced at the refill rate", bounded by the same 2 s
/// wait a Reactive read gets. Scheduled work (`gitpulsed`) keeps shedding.
pub fn run_process_awaiting_background_budget() {
    PROCESS_BACKGROUND_AWAITS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by [`run_process_with_unlimited_spawn_rate`]; never set in a shipped
/// binary.
static PROCESS_UNLIMITED_RATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lifts the spawn *rate* cap for the rest of this process. The concurrency
/// limit still applies.
///
/// For integration-test processes only. Each `tests/*.rs` crate links the
/// library without `cfg(test)`, so it otherwise runs under the production
/// rate; a stress test that starts hundreds of children then measures that
/// rate instead of the code under test. (Such a process never reaches the
/// per-user shared budget either way: only the shipped binaries opt into it,
/// see [`run_process_with_shared_spawn_budget`].) The unit-test build gets
/// the same gate from `cfg!(test)`. Order-independent: it takes effect at the
/// next spawn, even if the gate already exists. `tests/process_admission.rs`
/// fails if any source under `src/` calls it.
#[doc(hidden)]
pub fn run_process_with_unlimited_spawn_rate() {
    PROCESS_UNLIMITED_RATE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by [`run_process_with_shared_spawn_budget`], which only the shipped
/// binaries call.
static PROCESS_SHARED_BUDGET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Makes this process draw from the per-user spawn budget every GitPulse
/// process of this user shares (see [`shared_budget`]), as well as its own.
///
/// Opt-in, so that only the processes the budget exists for spend it: the
/// app, `gitpulse-mcp`, `gitpulsed` and `gitpulse-hook` each call it at the
/// top of `main`. Everything else that links this library — every
/// `tests/*.rs` crate and bench, which get the production gate because they
/// build without `cfg(test)` — keeps a per-process budget by construction,
/// rather than spending the running app's and being deferred by it.
/// `tests/process_admission.rs` derives the binary list from `Cargo.toml` and
/// `src/bin/` and fails if one does not make the call. A mobile build would
/// enter through `run()` rather than `main` and stay per-process; none ships.
///
/// The record is opened when the gate is built, by the first spawn, so a
/// call after that could not take effect: it panics rather than leave the
/// process silently off the budget. Absent from the unit-test build, where
/// opting in would open the real record under the developer's `HOME`.
#[cfg(not(test))]
pub fn run_process_with_shared_spawn_budget() {
    assert!(
        SPAWN_GATE.get().is_none(),
        "run_process_with_shared_spawn_budget must run before the first spawn: \
         the spawn gate is already built on a per-process budget"
    );
    PROCESS_SHARED_BUDGET.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The class a thread starts in: see [`run_process_as_background`].
fn thread_default_admission() -> Admission {
    if PROCESS_BACKGROUND.load(std::sync::atomic::Ordering::Relaxed) {
        Admission::Background
    } else {
        Admission::Reactive
    }
}

thread_local! {
    static ADMISSION: Cell<Admission> = Cell::new(thread_default_admission());
    /// Whether this thread's background spawns wait for budget; a thread
    /// starts in the process default, like [`ADMISSION`].
    static AWAITS_BUDGET: Cell<bool> =
        Cell::new(PROCESS_BACKGROUND_AWAITS.load(std::sync::atomic::Ordering::Relaxed));
    /// The repository the guarded mutation running on this thread named, so
    /// the refresh that follows it can be credited (see [`run_command_scope`]).
    static MARKED_REPO: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

/// The class the current thread's spawns are admitted under. A new thread
/// starts in [`thread_default_admission`]; a body that hops threads carries
/// this value across with [`with_admission`].
pub(crate) fn current_admission() -> Admission {
    ADMISSION.get()
}

/// [`current_admission`] by name, for diagnostics and the process-default
/// contract test, which runs in a process of its own.
pub fn current_admission_name() -> &'static str {
    current_admission().name()
}

/// Runs `body` under `class`, restoring the previous class on return, error
/// and unwinding, so a pooled worker never keeps a class into its next task.
///
/// Entering [`Admission::Background`] from another class also runs the thread
/// at UTILITY QoS for the span (macOS; see `pools::UtilityScope`). The class is
/// the one decision about whether work may be starved, so it drives the
/// thread's scheduling class as well as the children's ([`runs_in_background_band`]).
pub(crate) fn with_admission<T>(class: Admission, body: impl FnOnce() -> T) -> T {
    struct Restore(Admission);
    impl Drop for Restore {
        fn drop(&mut self) {
            ADMISSION.set(self.0);
        }
    }
    let previous = ADMISSION.replace(class);
    let _restore = Restore(previous);
    let _qos = (class == Admission::Background && previous != Admission::Background)
        .then(crate::pools::UtilityScope::enter);
    // User-facing work nested in a background scope gets the thread's own
    // class back for its span, never more than the thread had outside.
    let _user_facing = (class != Admission::Background && previous == Admission::Background)
        .then(crate::pools::UserFacingScope::enter);
    body()
}

thread_local! {
    /// Depth of [`with_background_band`] scopes on this thread.
    static BAND_FORCED: Cell<u32> = const { Cell::new(0) };
}

/// Runs `body` with every child it spawns in the background band whatever the
/// admission class, for work that is background by what it is: `devmap build`
/// is an index rebuild that has run at 50-78% CPU and 1.3 GB, and a user
/// asking for one still does not want their typing to wait behind it.
/// Admission is untouched, so the gate still admits it as its caller's class.
pub(crate) fn with_background_band<T>(body: impl FnOnce() -> T) -> T {
    struct Leave;
    impl Drop for Leave {
        fn drop(&mut self) {
            BAND_FORCED.set(BAND_FORCED.get() - 1);
        }
    }
    BAND_FORCED.set(BAND_FORCED.get() + 1);
    let _leave = Leave;
    body()
}

#[cfg(all(test, target_os = "macos"))]
thread_local! {
    /// `(entered, failed)` for the spawns made on this thread, see
    /// [`band_outcomes`].
    static BAND_OUTCOMES: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// Counts what the spawn of a background-band child did, so a refused band is
/// visible to a test as well as in the error the caller gets. A refusal is not
/// logged here: it is the spawn's failure, and travels to the caller as one.
#[cfg(all(test, target_os = "macos"))]
fn count_band_spawn<T>(spawned: &std::io::Result<T>) {
    let (entered, failed) = BAND_OUTCOMES.get();
    match spawned {
        Ok(_) => BAND_OUTCOMES.set((entered + 1, failed)),
        Err(error) if crate::procguard::band_refusal(error) => {
            BAND_OUTCOMES.set((entered, failed + 1));
        }
        Err(_) => {}
    }
}

/// `(entered, failed)`: background-band children that started in the band, and
/// spawns that failed because the band could not be entered, counted for the
/// calling thread. Per thread, not per process, so a test that makes the band
/// fail cannot show up in another test's difference.
#[cfg(all(test, target_os = "macos"))]
pub(crate) fn band_outcomes() -> (u64, u64) {
    BAND_OUTCOMES.get()
}

/// Whether a child spawned under `class` on this thread goes into the macOS
/// background band: exactly the children of [`Admission::Background`], plus
/// those inside [`with_background_band`]. A user action (`Interactive`) and a
/// read someone is waiting on (`Reactive`) are never starved, so a mis-classed
/// write is slow at worst, never throttled.
fn runs_in_background_band(class: Admission) -> bool {
    class == Admission::Background || BAND_FORCED.get() > 0
}

/// Runs `body` with background spawns waiting for budget (`awaits`) or shed
/// at once, restoring the thread's previous setting on return and unwinding.
#[cfg(test)]
pub(crate) fn with_background_budget_wait<T>(awaits: bool, body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            AWAITS_BUDGET.set(self.0);
        }
    }
    let _restore = Restore(AWAITS_BUDGET.replace(awaits));
    body()
}

/// Applies only inside a synchronous blocking operation.
pub(crate) fn with_background_processes<T>(body: impl FnOnce() -> T) -> T {
    with_admission(Admission::Background, body)
}

/// Marks the rest of the enclosing [`with_admission`] scope as a user action.
/// The command guard calls this once per mutation, and the command runner
/// scopes every command body, so the mark ends with the command that set it.
pub(crate) fn mark_interactive() {
    ADMISSION.set(Admission::Interactive);
}

/// [`mark_interactive`] for a mutation on `repo_path`, which also names the
/// repository whose follow-up refresh [`run_command_scope`] credits.
pub(crate) fn mark_user_action(repo_path: &str) {
    mark_interactive();
    let repo = std::fs::canonicalize(repo_path).unwrap_or_else(|_| PathBuf::from(repo_path));
    MARKED_REPO.with(|slot| *slot.borrow_mut() = Some(repo));
}

/// Runs one IPC command body.
///
/// Every command starts `Reactive`, and the guard promotes the rest of a
/// mutation's body to a user action. When the body ends, the promotion ends
/// with it — the pool thread never carries it into its next task — and two
/// things follow from a body that was a user action:
///
/// * Its error never carries the deferral marker out ([`not_retryable`]). The
///   frontend retries a deferred call on the strength of "nothing ran"; a
///   call that reached the mutation guard may already have changed things,
///   for instance when a thread it hopped to was deferred mid-action.
/// * The repository it named gets a short post-action credit: the refresh the
///   frontend asks for next is admitted like the action that caused it,
///   rather than deferred behind the refresh traffic the action itself set
///   off. See [`POST_ACTION_CREDIT_SPAWNS`].
pub(crate) fn run_command_scope<T>(
    body: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    struct Scope {
        previous: Admission,
        previous_repo: Option<PathBuf>,
    }
    impl Drop for Scope {
        fn drop(&mut self) {
            ADMISSION.set(self.previous);
            let previous = self.previous_repo.take();
            MARKED_REPO.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let _scope = Scope {
        previous: ADMISSION.replace(Admission::Reactive),
        previous_repo: MARKED_REPO.with(|slot| slot.borrow_mut().take()),
    };
    let result = body();
    if current_admission() != Admission::Interactive {
        return result;
    }
    if let Some(repo) = MARKED_REPO.with(|slot| slot.borrow_mut().take()) {
        grant_post_action_credit(&repo, Instant::now());
    }
    result.map_err(|error| error.map_message(not_retryable))
}

/// Rewrites a deferral so it no longer reads as one. The cause stays in the
/// sentence; only the marker that means "nothing ran, ask again" goes.
fn not_retryable(message: String) -> String {
    if !is_deferred_under_load(&message) {
        return message;
    }
    message.replacen(
        DEFERRED_MARKER,
        " was deferred under load (the action had already started, so it was not retried) after ",
        1,
    )
}

/// Spawns in the acted-on repository that a user action's follow-up refresh
/// may make without a rate token, and for how long. Sized for one full
/// snapshot (status, branches, tags, stash, operation probe, numstat, default
/// branch probes) with headroom, and short enough that a watcher storm that
/// starts after the action is back under the rate limit within seconds.
/// Inferred from the hydrate read set, not measured per repository.
const POST_ACTION_CREDIT_SPAWNS: u32 = 32;
const POST_ACTION_CREDIT_WINDOW: Duration = Duration::from_secs(3);
/// Repositories holding a live credit at once. A user acts on one repository
/// at a time; the bound only stops a scripted burst growing the map.
const POST_ACTION_CREDIT_REPOS: usize = 64;

struct PostActionCredit {
    remaining: u32,
    until: Instant,
}

fn post_action_credits() -> &'static Mutex<HashMap<PathBuf, PostActionCredit>> {
    static CREDITS: OnceLock<Mutex<HashMap<PathBuf, PostActionCredit>>> = OnceLock::new();
    CREDITS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grant_post_action_credit(repo: &Path, now: Instant) {
    let mut credits = post_action_credits()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    credits.retain(|_, credit| credit.until > now);
    if credits.len() >= POST_ACTION_CREDIT_REPOS && !credits.contains_key(repo) {
        if let Some(oldest) = credits
            .iter()
            .min_by_key(|(_, credit)| credit.until)
            .map(|(path, _)| path.clone())
        {
            credits.remove(&oldest);
        }
    }
    credits.insert(
        repo.to_path_buf(),
        PostActionCredit {
            remaining: POST_ACTION_CREDIT_SPAWNS,
            until: now + POST_ACTION_CREDIT_WINDOW,
        },
    );
}

/// Credits the reads a tab activation is about to make in `repo_path`, exactly
/// as a mutation's follow-up refresh is credited. Trust-checked through
/// [`validate_repo`] first, which also yields the canonical path the spawn
/// side matches the credit on; an untrusted or missing repository earns none.
///
/// Switching tabs is a user action, but its hydrate is all reads, so it ran
/// `Reactive` and queued behind whatever refresh traffic was spending the rate
/// budget — the click the user was waiting on lost to work nobody was looking
/// at. The same bounded credit keeps a scripted stream of activations from
/// becoming unmetered: [`POST_ACTION_CREDIT_SPAWNS`] per activation, for
/// [`POST_ACTION_CREDIT_WINDOW`], and only in that one repository.
pub(crate) fn note_tab_activated(repo_path: &str) -> Result<(), CommandError> {
    let repo = validate_repo(repo_path)?;
    grant_post_action_credit(&repo, Instant::now());
    Ok(())
}

/// Spends one credit for a spawn whose working directory is `cwd`.
fn take_post_action_credit(cwd: &Path, now: Instant) -> bool {
    let mut credits = post_action_credits()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let Some(credit) = credits.get_mut(cwd) else {
        return false;
    };
    if credit.until <= now || credit.remaining == 0 {
        credits.remove(cwd);
        return false;
    }
    credit.remaining -= 1;
    true
}

/// Why the gate started nothing. Each cause is its own variant because each
/// means something different to the person reading it: a deferral is the
/// app protecting itself and says how long it tried; a timeout is a slot that
/// never came free inside the command's own deadline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Refusal {
    /// A reactive spawn waited [`SPAWN_QUEUE_BUDGET`] for a rate token.
    Refused {
        waited: Duration,
    },
    /// Background work refused without waiting: the rate budget was at or
    /// below the reserve kept for reactive reads.
    Shed,
    /// Refused without waiting because the host is under memory pressure and
    /// this class is shed while it lasts (`sysload::governor`). Carries the
    /// deferral marker, so the frontend retries it once the pressure clears.
    Pressure {
        reactive: bool,
    },
    /// No concurrency slot became free before the command's own deadline.
    TimedOut {
        deadline: Duration,
    },
    Cancelled,
}

/// What the gate did, per class, since launch. Exported with diagnostics so
/// "user actions failed" can be told apart from "refreshes were deferred".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdmissionCounters {
    pub admitted: u64,
    /// Joined an identical queued read instead of starting a child.
    pub coalesced: u64,
    pub refused: u64,
    pub shed: u64,
    pub timed_out: u64,
    pub cancelled: u64,
}

impl AdmissionCounters {
    fn count(&mut self, refusal: Refusal) {
        match refusal {
            Refusal::Refused { .. } => self.refused += 1,
            Refusal::Shed | Refusal::Pressure { .. } => self.shed += 1,
            Refusal::TimedOut { .. } => self.timed_out += 1,
            Refusal::Cancelled => self.cancelled += 1,
        }
    }
}

/// A counting semaphore over live child processes.
///
/// Deliberately plain `std` rather than a new dependency: the only operations
/// needed are "wait for a slot" and "give one back", and the wait happens on
/// threads that are already blocking on a subprocess.
struct SpawnGate {
    limit: usize,
    /// `u32::MAX` turns the token bucket off. The process-wide gate does that
    /// under `cfg(test)` so the suite can spawn git freely; production uses
    /// [`production_spawn_rate`].
    burst: u32,
    refill_per_sec: u32,
    /// The budget every GitPulse process of this user also draws from, when
    /// it could be opened (see [`shared_budget`]). `None` for test gates,
    /// and for a production gate whose record was unusable — `shared_note`
    /// then says why, so a per-process budget is never reported as shared.
    shared: Option<shared_budget::SharedBudget>,
    shared_note: Option<String>,
    state: Mutex<GateState>,
    released: Condvar,
}

#[derive(Default)]
struct GateState {
    in_flight: usize,
    background: usize,
    waiting_background: usize,
    /// Interactive callers waiting for a slot. While any wait, the next free
    /// slot is theirs: a user action never queues behind a refresh.
    waiting_interactive: usize,
    counters: [AdmissionCounters; 3],
    /// High-water mark, kept so a test can assert the ceiling actually held
    /// rather than assert on the counter it is trying to prove bounded.
    peak: usize,
    /// Whole tokens remaining. Integer so a fractional refill cannot admit an
    /// extra child, and so concurrent waiters cannot share one token via float
    /// rounding. The mutex makes the decrement atomic with the check.
    tokens: u32,
    tokens_at: Option<Instant>,
    /// Decisions that could not consult the shared budget and fell back to
    /// this process's own (a lock held past its patience, an I/O error).
    shared_fallbacks: u64,
    /// Decisions the shared budget denied but could not record. Refused like
    /// any denial (and so also in `shed` / `deferred`), counted here as well so
    /// a record that has stopped taking writes shows in the report instead of
    /// only in a log line that is throttled.
    shared_unrecorded_denials: u64,
    /// Admitted children by what they were (`git status`, `gh`, ...). The
    /// class totals said the budget was spent; this says on what, which is
    /// the question every refusal raised and no log could answer.
    by_label: LabelCounts,
}

/// Admissions per spawn label, bounded so a stream of novel labels cannot
/// grow it: past [`LabelCounts::MAX_LABELS`] distinct labels, the rest are
/// counted under [`LabelCounts::OTHER`].
#[derive(Default)]
struct LabelCounts {
    counts: HashMap<String, u64>,
}

impl LabelCounts {
    const MAX_LABELS: usize = 64;
    const OTHER: &'static str = "other";
    /// Labels printed per report; the rest are summed into one figure.
    const REPORTED: usize = 8;

    fn note(&mut self, label: &str) {
        let key = report_label(label);
        if let Some(count) = self.counts.get_mut(&key) {
            *count += 1;
            return;
        }
        let key = if self.counts.len() >= Self::MAX_LABELS {
            Self::OTHER.to_string()
        } else {
            key
        };
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// `top: git status=310 git for-each-ref=220 ... rest=41`, largest first,
    /// ties by name so the line is stable.
    fn report(&self) -> String {
        if self.counts.is_empty() {
            return "top: none".to_string();
        }
        let mut rows: Vec<(&String, &u64)> = self.counts.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let shown: Vec<String> = rows
            .iter()
            .take(Self::REPORTED)
            .map(|(label, count)| format!("{label}={count}"))
            .collect();
        let rest: u64 = rows
            .iter()
            .skip(Self::REPORTED)
            .map(|(_, count)| **count)
            .sum();
        if rest == 0 {
            format!("top: {}", shown.join(" "))
        } else {
            format!("top: {} rest={rest}", shown.join(" "))
        }
    }
}

/// A spawn label reduced to what may be written to a diagnostics log: its
/// first two words, each kept only if it looks like a program or subcommand
/// name. A label is caller-built and could carry a path or a ref name; those
/// become `…`, so attribution can never leak repository contents.
fn report_label(label: &str) -> String {
    let words: Vec<&str> = label
        .split_whitespace()
        .take(2)
        .map(|word| {
            let plain = !word.is_empty()
                && word.len() <= 32
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                && !word.starts_with('.');
            if plain {
                word
            } else {
                "…"
            }
        })
        .collect();
    if words.is_empty() {
        "…".to_string()
    } else {
        words.join(" ")
    }
}

impl SpawnGate {
    fn new(limit: usize) -> Self {
        Self::with_rate(limit, u32::MAX, u32::MAX)
    }

    fn with_rate(limit: usize, burst: u32, refill_per_sec: u32) -> Self {
        let unlimited = refill_per_sec == u32::MAX;
        // A zero rate would wait out every deadline and look like a hung git.
        // Unlimited (`u32::MAX`) is the test-suite gate and must stay unlimited.
        let burst = if unlimited { burst } else { burst.max(1) };
        let refill_per_sec = if unlimited {
            refill_per_sec
        } else {
            refill_per_sec.max(1)
        };
        Self {
            limit: limit.max(1),
            burst,
            refill_per_sec,
            shared: None,
            shared_note: None,
            state: Mutex::new(GateState {
                tokens: if unlimited { 0 } else { burst },
                ..GateState::default()
            }),
            released: Condvar::new(),
        }
    }

    /// Also draws every rate token from the record at `path`, shared with
    /// every other process that opens it. An unusable record leaves the gate
    /// on its own bucket and keeps the reason, so the report says so.
    fn sharing(mut self, path: Result<PathBuf, String>) -> Self {
        if self.refill_per_sec == u32::MAX {
            self.shared_note = Some("unlimited gate: no rate budget to share".into());
            return self;
        }
        match path.and_then(|path| shared_budget::SharedBudget::open(&path)) {
            Ok(shared) => {
                self.shared_note = None;
                self.shared = Some(shared);
            }
            Err(reason) => {
                // Not opting in is the intended state of every process that
                // is not a shipped binary, not a fault worth a warning.
                if reason != SHARED_BUDGET_NOT_OPTED_IN {
                    log::warn!(
                        target: "spawn_gate",
                        "spawn rate budget is per-process only: {reason}"
                    );
                }
                self.shared_note = Some(reason);
                self.shared = None;
            }
        }
        self
    }

    /// Asks the shared budget for the token a non-interactive spawn is about
    /// to spend. Does file IO and may wait on a lock another process holds, so
    /// it must be called with the gate mutex released; the answer is applied
    /// by [`Self::settle_shared`].
    fn ask_shared(&self, class: Admission) -> (shared_budget::Take, Option<u32>) {
        let Some(shared) = &self.shared else {
            return (shared_budget::Take::Granted, None);
        };
        shared.take_leveled(self.floor_for(class), self.burst, self.refill_per_sec)
    }

    /// The tokens a spawn of `class` may not spend: none for reactive work,
    /// the reserve for background work.
    fn floor_for(&self, class: Admission) -> u32 {
        if class == Admission::Background {
            self.background_reserve()
        } else {
            0
        }
    }

    fn level_of(&self, bucket: RefusingBucket, tokens: u32, class: Admission) -> BucketLevel {
        BucketLevel {
            bucket,
            tokens,
            floor: self.floor_for(class),
            burst: self.burst,
        }
    }

    /// Applies [`Self::ask_shared`]'s answer under the gate lock: `true` when
    /// the token was granted or the record could not be consulted (counted,
    /// and logged at most every [`REFUSAL_REPORT_INTERVAL`]); `false` when the
    /// processes together have spent it, whether or not the record could be
    /// rewritten to say so (counted apart, in `shared_unrecorded_denials`).
    fn settle_shared(&self, state: &mut GateState, taken: shared_budget::Take) -> bool {
        match taken {
            shared_budget::Take::Granted => true,
            shared_budget::Take::Denied => false,
            shared_budget::Take::DeniedUnrecorded(reason) => {
                state.shared_unrecorded_denials = state.shared_unrecorded_denials.saturating_add(1);
                log_unrecorded_denial(&reason, state.shared_unrecorded_denials);
                false
            }
            shared_budget::Take::Unavailable(reason) => {
                state.shared_fallbacks = state.shared_fallbacks.saturating_add(1);
                log_shared_fallback(&reason, state.shared_fallbacks);
                true
            }
        }
    }

    /// One line on where this gate's rate budget lives, for the report.
    fn sharing_report(&self, fallbacks: u64, unrecorded_denials: u64) -> String {
        match (&self.shared, &self.shared_note) {
            (Some(shared), _) => format!(
                "budget=shared({}) unrecorded_denials={unrecorded_denials} fallbacks={fallbacks}",
                shared.path().display()
            ),
            (None, Some(note)) => format!("budget=per-process ({note})"),
            (None, None) => "budget=per-process".to_string(),
        }
    }

    fn refill(&self, state: &mut GateState, now: Instant) {
        if self.refill_per_sec == u32::MAX {
            return;
        }
        let Some(at) = state.tokens_at else {
            state.tokens_at = Some(now);
            return;
        };
        let elapsed_ms = now.saturating_duration_since(at).as_millis();
        if elapsed_ms == 0 {
            return;
        }
        // Keep the unused remainder on `tokens_at` so 249 ms at 4/s does not
        // become a token, and the next 1 ms does.
        let add = elapsed_ms.saturating_mul(u128::from(self.refill_per_sec)) / 1000;
        if add == 0 {
            return;
        }
        let add_u = u32::try_from(add).unwrap_or(u32::MAX);
        state.tokens = state.tokens.saturating_add(add_u).min(self.burst);
        let consumed_ms = add.saturating_mul(1000) / u128::from(self.refill_per_sec);
        let consumed = Duration::from_millis(u64::try_from(consumed_ms).unwrap_or(u64::MAX));
        state.tokens_at = Some(at.checked_add(consumed).unwrap_or(now));
    }

    #[cfg(test)]
    fn acquire(&self, deadline: Instant) -> Option<SpawnPermit<'_>> {
        self.acquire_until(deadline, &|| false).ok()
    }

    /// [`Self::acquire_judged`] without the level of the bucket that refused.
    #[cfg(test)]
    fn acquire_until(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SpawnPermit<'_>, Refusal> {
        self.acquire_judged(deadline, cancelled)
            .map_err(|(refusal, _)| refusal)
    }

    /// Tokens background work may not spend: a quarter of the burst, so
    /// optional work is shed while reactive reads still have budget.
    /// How long one token takes to refill: the soonest the shared record can
    /// answer differently than it just did.
    fn refill_interval(&self) -> Duration {
        Duration::from_millis(1000_u64.div_ceil(u64::from(self.refill_per_sec.max(1))))
    }

    fn background_reserve(&self) -> u32 {
        self.burst / 4
    }

    /// Waits for a slot and a rate token until `deadline`. A refusal that was
    /// the rate budget's carries the bucket that refused and its level, for
    /// [`refusal_message_at`].
    fn acquire_judged(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SpawnPermit<'_>, (Refusal, Option<BucketLevel>)> {
        let class = current_admission();
        let background = class == Admission::Background;
        let interactive = class == Admission::Interactive;
        let awaits_budget = background && AWAITS_BUDGET.get();
        // The load governor's verdict, one owner (`sysload::governor`): under
        // memory pressure these classes are refused at once with the
        // deferral marker. A user action is never shed, and neither is a read
        // credited to one (`take_post_action_credit` promoted it already).
        if !interactive {
            let shed = crate::sysload::shed();
            if (background && shed.background) || (!background && shed.reactive) {
                let refusal = Refusal::Pressure {
                    reactive: !background,
                };
                self.state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .counters[class.index()]
                .count(refusal);
                return Err((refusal, None));
            }
        }
        let rate_limited = self.refill_per_sec != u32::MAX
            && !PROCESS_UNLIMITED_RATE.load(std::sync::atomic::Ordering::Relaxed);
        let entered = Instant::now();
        // Declared before the state guard so that, unwinding, the guard is
        // released first and the reservation can take the lock to give its
        // waiting count back. A stranded interactive or background count
        // would make every later refresh ineligible for good.
        let mut reservation = WaitReservation {
            gate: self,
            class,
            held: false,
        };
        // A poisoned gate must not deadlock the app: a panic inside a permit
        // holder still ran the `Drop` below, so the count is accurate.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if background {
            state.waiting_background += 1;
            self.released.notify_all();
        }
        if interactive {
            state.waiting_interactive += 1;
        }
        reservation.held = background || interactive;
        let mut token_wait_started: Option<Instant> = None;
        // When this waiter may next ask the shared record. A record changes at
        // most once per refill interval, so a waiter that was denied has
        // nothing to learn from asking sooner, and each ask is a locked read
        // and rewrite of a file every process shares.
        let mut next_shared_ask: Option<Instant> = None;
        // A token the shared budget granted while the gate lock was released.
        // Kept for the next pass, which re-reads the gate before spending it:
        // the slot it was asked for may have gone to another waiter meanwhile.
        // One that is never spent (the waiter is cancelled or times out first)
        // is lost to the shared budget, which has no way to take it back.
        let mut shared_token_held = false;
        // What the shared record held the last time it denied this waiter, and
        // which bucket was short the last time this waiter was refused a
        // token: what a refusal names.
        let mut shared_denial: Option<BucketLevel> = None;
        let mut short: Option<BucketLevel> = None;
        let outcome = loop {
            // The cancel check is the caller's code. Under the gate lock, a
            // check that took a lock of its own or spawned anything would
            // deadlock the whole gate, so it runs with the lock released and
            // everything below is recomputed from fresh state.
            drop(state);
            let is_cancelled = cancelled();
            state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = Instant::now();
            let remaining = deadline.saturating_duration_since(now);
            if is_cancelled {
                break Err((Refusal::Cancelled, None));
            }
            if remaining.is_zero() {
                // Whatever the caller was last waiting for is the cause. A
                // short deadline that ran out while the rate budget was spent
                // is a deferral, not a slot that never came free.
                break Err(match token_wait_started {
                    Some(_) => (
                        Refusal::Refused {
                            waited: now.saturating_duration_since(entered),
                        },
                        short,
                    ),
                    None => (
                        Refusal::TimedOut {
                            deadline: deadline.saturating_duration_since(entered),
                        },
                        None,
                    ),
                });
            }
            // Optional work gets at most a quarter of the slots (at least
            // one). Conversely, a waiting background class gets the next
            // available slot if no background child is running, so sustained
            // reactive traffic cannot indefinitely postpone all indexing. A
            // waiting user action outranks both.
            let eligibility = |state: &GateState| match class {
                Admission::Interactive => true,
                Admission::Background => {
                    state.waiting_interactive == 0 && state.background < (self.limit / 4).max(1)
                }
                Admission::Reactive => {
                    state.waiting_interactive == 0
                        && (state.waiting_background == 0
                            || state.background > 0
                            || state.in_flight < self.limit.saturating_sub(1))
                }
            };
            let tokens_available = |state: &GateState| match class {
                Admission::Interactive => true,
                Admission::Reactive => !rate_limited || state.tokens >= 1,
                Admission::Background => !rate_limited || state.tokens > self.background_reserve(),
            };
            let mut eligible = eligibility(&state);
            self.refill(&mut state, now);
            let mut tokens_ok = tokens_available(&state);
            // The shared budget is asked last, and only by a spawn that would
            // otherwise start now, so a granted token is spent at once below
            // and never taken for a waiter that then gives up.
            let asks_shared = tokens_ok
                && rate_limited
                && !interactive
                && !shared_token_held
                && state.in_flight < self.limit
                && eligible
                && self.shared.is_some();
            // A waiter (anything that will not be refused at once) is paced to
            // the refill interval; a first ask, or a spawn that is refused on
            // its answer, is not delayed.
            let paced = asks_shared
                && (!background || awaits_budget)
                && next_shared_ask.is_some_and(|at| now < at);
            if paced {
                tokens_ok = false;
            } else if asks_shared {
                // The shared record is a file under a lock that may sleep-poll
                // for [`shared_budget::LOCK_PATIENCE`], and a denial writes it
                // too. None of that may happen under the gate mutex, where it
                // would queue every other acquire, a user action included,
                // behind a background re-poll. Released like the cancel check
                // above, and everything is re-read once the answer is in.
                drop(state);
                let (taken, shared_level) = self.ask_shared(class);
                state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if self.settle_shared(&mut state, taken) {
                    shared_token_held = true;
                    // The lock was released: another waiter may have taken the
                    // slot or the local token, so both are read again. The
                    // deadline is not: a token already granted is spent, not
                    // thrown away because the wait for it ran long.
                    eligible = eligibility(&state);
                    tokens_ok = tokens_available(&state);
                } else {
                    tokens_ok = false;
                    next_shared_ask = Some(Instant::now() + self.refill_interval());
                    shared_denial = shared_level
                        .map(|tokens| self.level_of(RefusingBucket::Shared, tokens, class));
                }
            }
            // An awaiting background reader counts in `waiting_background`
            // only while it waits for a SLOT, which is what the count reserves
            // a slot for (see `eligibility`). One short of tokens cannot use a
            // slot, so it must not keep one from a reactive read that can.
            if awaits_budget {
                if reservation.held && !tokens_ok {
                    state.waiting_background = state.waiting_background.saturating_sub(1);
                    reservation.held = false;
                    self.released.notify_all();
                } else if !reservation.held && tokens_ok {
                    state.waiting_background += 1;
                    reservation.held = true;
                }
            }
            if !tokens_ok {
                // The process bucket is short first; past it, the record is.
                short = if tokens_available(&state) {
                    shared_denial
                } else {
                    Some(self.level_of(RefusingBucket::Local, state.tokens, class))
                };
                // Background is shed at once: it is the first thing to go
                // under load, and waiting would only hold a pool thread.
                // Unless this process asked to wait (an agent's MCP read: see
                // [`run_process_awaiting_background_budget`]), which then
                // waits like a reactive read, still above the reserve.
                if background && !awaits_budget {
                    break Err((Refusal::Shed, short));
                }
                let started = *token_wait_started.get_or_insert(now);
                let waited = now.saturating_duration_since(started);
                // A storm that has spent the burst must fail the spawn instead
                // of occupying a blocking-pool thread for the whole git timeout.
                if waited >= SPAWN_QUEUE_BUDGET {
                    break Err((
                        Refusal::Refused {
                            waited: now.saturating_duration_since(entered),
                        },
                        short,
                    ));
                }
            } else {
                token_wait_started = None;
            }
            if state.in_flight < self.limit && eligible && tokens_ok {
                break Ok(());
            }
            // Wake when the next token is due rather than on a fixed poll: a
            // waiter refused a token has nothing to learn before then. The
            // wait is still capped at the cancellation poll, and at the
            // deadline.
            // The shared ask time only says when to wake while it is still in
            // the future; once past, whichever bucket is refusing now (the
            // local one, say) refills on the same interval.
            let nap = match (tokens_ok, next_shared_ask) {
                (false, Some(at)) if at > now => at.saturating_duration_since(now),
                (false, _) if rate_limited => self.refill_interval(),
                _ => CANCEL_POLL,
            }
            .max(MIN_TOKEN_NAP);
            let (next, _) = self
                .released
                .wait_timeout(state, remaining.min(nap.min(CANCEL_POLL)))
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            state = next;
        };
        reservation.give_back(&mut state);
        if let Err((refusal, level)) = outcome {
            state.counters[class.index()].count(refusal);
            let counters = state.counters;
            let sharing =
                self.sharing_report(state.shared_fallbacks, state.shared_unrecorded_denials);
            drop(state);
            // A refused waiter must release its reservation.
            self.released.notify_all();
            if refusal != Refusal::Cancelled {
                // Rendered only when the throttle lets a line out: refusals
                // come in storms, and sorting the labels under the gate lock
                // for every one would tax exactly the path being diagnosed.
                log_refusals(&counters, &sharing, level, || self.label_report());
            }
            return Err((refusal, level));
        }
        state.in_flight += 1;
        if background {
            state.background += 1;
        }
        if rate_limited && !interactive {
            state.tokens = state.tokens.saturating_sub(1);
        }
        state.counters[class.index()].admitted += 1;
        state.peak = state.peak.max(state.in_flight);
        Ok(SpawnPermit {
            gate: self,
            background,
        })
    }

    /// The `top: ...` attribution line, rendered under a brief lock.
    fn label_report(&self) -> String {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_label
            .report()
    }

    /// Attributes one admitted child to its label for the gate report.
    fn note_admitted_label(&self, label: &str) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_label
            .note(label);
    }

    fn note_coalesced(&self, class: Admission) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counters[class.index()]
        .coalesced += 1;
    }

    /// Takes back a [`Self::note_coalesced`] for a joiner whose leader timed
    /// out and who asks again: it was not answered by another's child.
    fn note_uncoalesced(&self, class: Admission) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let counters = &mut state.counters[class.index()];
        counters.coalesced = counters.coalesced.saturating_sub(1);
    }

    #[cfg(test)]
    fn counters(&self) -> [AdmissionCounters; 3] {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counters
    }

    #[cfg(test)]
    fn peak(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .peak
    }
}

/// A waiter's place in `waiting_interactive` / `waiting_background`. Given
/// back under the caller's lock on every normal exit, and by `Drop` when the
/// waiter unwinds, so a panic anywhere in the wait cannot strand it.
struct WaitReservation<'a> {
    gate: &'a SpawnGate,
    class: Admission,
    held: bool,
}

impl WaitReservation<'_> {
    fn give_back(&mut self, state: &mut GateState) {
        if !std::mem::take(&mut self.held) {
            return;
        }
        match self.class {
            Admission::Interactive => {
                state.waiting_interactive = state.waiting_interactive.saturating_sub(1)
            }
            Admission::Background => {
                state.waiting_background = state.waiting_background.saturating_sub(1)
            }
            Admission::Reactive => {}
        }
    }
}

impl Drop for WaitReservation<'_> {
    fn drop(&mut self) {
        if !self.held {
            return;
        }
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.give_back(&mut state);
        drop(state);
        self.gate.released.notify_all();
    }
}

/// Releases its slot on drop, so an early `?` return, a timeout path and a
/// panic all give the descriptor budget back.
struct SpawnPermit<'a> {
    gate: &'a SpawnGate,
    background: bool,
}

impl Drop for SpawnPermit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
        if self.background {
            state.background = state.background.saturating_sub(1);
        }
        drop(state);
        // Waking only an ineligible class can strand an otherwise free slot.
        self.gate.released.notify_all();
    }
}

/// Concurrent children allowed: twice the core count, clamped so a 2-core CI
/// box still overlaps work and a 32-core workstation does not put 64 `git`
/// processes and ~2 GiB of potential drain buffers on the host at once.
fn spawn_limit() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_mul(2))
        .unwrap_or(SPAWN_LIMIT_FLOOR)
        .clamp(SPAWN_LIMIT_FLOOR, SPAWN_LIMIT_CEILING)
}

/// Sustained git spawns per second, and the burst that may start immediately.
///
/// The measured storm on 2026-10-02 was 40–80 short-lived `git` processes per
/// second for hours. Concurrency alone does not stop that: 16 children that
/// each live ~200 ms still start about 80 processes a second. The burst covers
/// one uncached branch-stat pass (`MAX_BRANCH_STAT_TARGETS` × 2); after that
/// the refill is the hard cap.
const SPAWN_BURST: u32 = 192;
const SPAWN_PER_SEC: u32 = 8;
/// How long a caller may block once the burst is spent. Longer than one
/// refill interval, short enough that a saturated storm does not pin the
/// blocking pool until the 90s git deadline.
pub(crate) const SPAWN_QUEUE_BUDGET: Duration = Duration::from_secs(2);

/// The longest a waiter sleeps before it re-checks for cancellation.
const CANCEL_POLL: Duration = Duration::from_millis(50);
/// The shortest a waiter naps for a token that is due, so one already due
/// cannot spin the loop.
const MIN_TOKEN_NAP: Duration = Duration::from_millis(1);

pub(crate) fn production_spawn_rate() -> (u32, u32) {
    (SPAWN_BURST, SPAWN_PER_SEC)
}

fn configured_spawn_gate(testing: bool) -> SpawnGate {
    if testing {
        SpawnGate::new(spawn_limit())
    } else {
        let (burst, rate) = production_spawn_rate();
        SpawnGate::with_rate(spawn_limit(), burst, rate).sharing(shared_budget_path())
    }
}

/// Why a production gate in a process that did not opt in stays per-process.
const SHARED_BUDGET_NOT_OPTED_IN: &str = "this process did not opt into the per-user spawn budget";

/// The per-user record, for a process that opted in (see
/// [`run_process_with_shared_spawn_budget`]). Every other process — the
/// unit-test build, integration tests, benches — never touches the real one:
/// a test that spends a burst would otherwise spend the running app's.
fn shared_budget_path() -> Result<PathBuf, String> {
    if PROCESS_SHARED_BUDGET.load(std::sync::atomic::Ordering::Relaxed) {
        shared_budget::default_path()
    } else {
        Err(SHARED_BUDGET_NOT_OPTED_IN.into())
    }
}

/// Every child the gate actually started, so a test can count spawns in its
/// own repository instead of inferring them from timing.
///
/// Kept per working directory. The suite shares one process, and a log shared
/// by every test is evicted by whichever tests spawn the most: a test counting
/// its own children by position (`spawns_in(cwd).len()` before and after) then
/// lost its oldest entries to its neighbours and counted short, more often the
/// longer it ran, which is to say under load. A test's directory is only ever
/// pushed to by that test, so its entries are evicted by nothing but its own
/// volume, and a directory that has gone quiet is the first to be dropped.
#[cfg(test)]
pub(crate) mod spawn_log {
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;

    use super::Admission;

    /// Entries kept for one directory, and directories kept at once.
    const PER_DIRECTORY: usize = 16_384;
    const DIRECTORIES: usize = 1_024;
    type Entry = (Vec<String>, Admission);

    struct Log {
        per_directory: usize,
        directories: usize,
        by_directory: HashMap<PathBuf, (u64, VecDeque<Entry>)>,
        touched: u64,
    }

    impl Log {
        fn bounded(per_directory: usize, directories: usize) -> Self {
            Self {
                per_directory,
                directories,
                by_directory: HashMap::new(),
                touched: 0,
            }
        }

        fn push(&mut self, cwd: &Path, entry: Entry) {
            self.touched += 1;
            let touched = self.touched;
            if !self.by_directory.contains_key(cwd) && self.by_directory.len() >= self.directories {
                // The directory nothing has spawned in for longest.
                if let Some(stale) = self
                    .by_directory
                    .iter()
                    .min_by_key(|(_, (last, _))| *last)
                    .map(|(dir, _)| dir.clone())
                {
                    self.by_directory.remove(&stale);
                }
            }
            let (last, entries) = self
                .by_directory
                .entry(cwd.to_path_buf())
                .or_insert_with(|| (touched, VecDeque::new()));
            *last = touched;
            if entries.len() == self.per_directory {
                entries.pop_front();
            }
            entries.push_back(entry);
        }

        fn entries(&self, cwd: &Path) -> Vec<Entry> {
            self.by_directory
                .get(cwd)
                .map(|(_, entries)| entries.iter().cloned().collect())
                .unwrap_or_default()
        }
    }

    static LOG: Mutex<Option<Log>> = Mutex::new(None);

    pub(super) fn record(cmd: &Command) {
        let Some(cwd) = cmd.get_current_dir() else {
            return;
        };
        let argv = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let class = super::current_admission();
        LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| Log::bounded(PER_DIRECTORY, DIRECTORIES))
            .push(cwd, (argv, class));
    }

    /// Argument vectors of every recorded child whose working directory was
    /// `cwd`, oldest first.
    pub(crate) fn spawns_in(cwd: &Path) -> Vec<Vec<String>> {
        classed_spawns_in(cwd)
            .into_iter()
            .map(|(argv, _)| argv)
            .collect()
    }

    /// [`spawns_in`], with the admission class of the thread that spawned
    /// each child.
    pub(crate) fn classed_spawns_in(cwd: &Path) -> Vec<(Vec<String>, Admission)> {
        LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|log| log.entries(cwd))
            .unwrap_or_default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entry() -> Entry {
            (vec!["status".into()], Admission::Reactive)
        }

        /// The defect: a count taken by position was shortened by other tests'
        /// spawns. One directory's volume no longer evicts another's entries.
        #[test]
        fn a_busy_neighbour_does_not_evict_a_quiet_directory() {
            let mut log = Log::bounded(8, 4);
            let (quiet, busy) = (Path::new("/quiet"), Path::new("/busy"));
            for _ in 0..3 {
                log.push(quiet, entry());
            }
            for _ in 0..100 {
                log.push(busy, entry());
            }
            assert_eq!(log.entries(quiet).len(), 3);
            assert_eq!(log.entries(busy).len(), 8, "bounded by its own volume");
        }

        #[test]
        fn the_directory_that_went_quiet_is_the_one_dropped() {
            let mut log = Log::bounded(8, 3);
            let (first, live) = (Path::new("/first"), Path::new("/live"));
            log.push(first, entry());
            log.push(live, entry());
            for other in ["/a", "/b", "/c", "/d"] {
                log.push(Path::new(other), entry());
                // A live directory keeps spawning, so it stays.
                log.push(live, entry());
            }
            assert!(log.entries(first).is_empty(), "the quiet one went");
            assert!(!log.entries(live).is_empty(), "the busy one stayed");
            assert_eq!(log.by_directory.len(), 3, "never more than its bound");
        }
    }
}

/// What makes two reads the same read: where, what, how much output, and the
/// class asking. The class is part of it so a user action never inherits a
/// refresh's refusal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct SharedReadKey {
    cwd: PathBuf,
    program: std::ffi::OsString,
    argv: Vec<std::ffi::OsString>,
    stdout_cap: usize,
    class: Admission,
}

/// A queued read that later identical reads may join. It leaves the table
/// the moment the gate admits it, before the child starts: a caller that
/// arrives after that point may be asking because of a write the running
/// child cannot have seen, so it starts its own.
struct SharedRead {
    result: Mutex<Option<Result<BoundedRun, CommandError>>>,
    done: Condvar,
    /// Where this flight's joiners go when its leader's deadline passed
    /// before theirs: the one flight that asks again for all of them, set by
    /// the first to arrive. It stays reachable from here after the gate has
    /// admitted it, because everyone who asks was waiting before its child
    /// started, which is all the "joined while queued" rule protects.
    retry: Mutex<Option<Arc<SharedRead>>>,
}

impl SharedRead {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            done: Condvar::new(),
            retry: Mutex::new(None),
        })
    }
}

fn shared_reads() -> &'static Mutex<HashMap<SharedReadKey, Arc<SharedRead>>> {
    static READS: OnceLock<Mutex<HashMap<SharedReadKey, Arc<SharedRead>>>> = OnceLock::new();
    READS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs a read-only `cmd`, sharing one child with every identical read still
/// queued at the gate. Only for commands that cannot change repository state:
/// a shared write would report one write's outcome as another's. Commands
/// without a working directory, or with stdin, are never shared.
///
/// `timeout` is this caller's own budget, whichever side of the flight it is
/// on. A joiner stops waiting for the leader's child when its own deadline
/// passes, with the timeout a solo call would report, and the leader runs on
/// untouched. A leader's timeout is the leader's: a joiner with budget left
/// runs its own read for what remains instead of adopting it.
fn run_read_shared(
    mut cmd: Command,
    label: &str,
    timeout: Duration,
    stdout_cap: usize,
    gate: &'static SpawnGate,
) -> Result<BoundedRun, CommandError> {
    let deadline = Instant::now() + timeout;
    let mut attempt = timeout;
    // The flight whose leader timed out last, and the longest prefix any such
    // leader printed: a keeping caller's rerun may reach its deadline with
    // less than that.
    let mut after: Option<Arc<SharedRead>> = None;
    let mut prefix: Option<BoundedRun> = None;
    loop {
        let outcome = match run_read_shared_once(
            &mut cmd,
            label,
            attempt,
            deadline,
            stdout_cap,
            gate,
            after.as_ref(),
        ) {
            SharedAttempt::Done(result) => result,
            SharedAttempt::LeaderTimedOut { flight, printed } => {
                if let Some(run) = printed {
                    if prefix
                        .as_ref()
                        .is_none_or(|kept| run.stdout.len() > kept.stdout.len())
                    {
                        prefix = Some(run);
                    }
                }
                after = Some(flight);
                attempt = deadline.saturating_duration_since(Instant::now());
                if attempt.is_zero() {
                    deadline_expired(label, timeout)
                } else {
                    continue;
                }
            }
        };
        return if after.is_some() {
            as_the_callers_deadline(outcome, prefix, label, timeout)
        } else {
            outcome
        };
    }
}

/// How one pass through the shared-read queue ended.
enum SharedAttempt {
    Done(Result<BoundedRun, CommandError>),
    /// The leader this caller joined ran out of its own time while this
    /// caller's deadline has not come. Its timeout is not this caller's
    /// answer, so the caller asks again, on `flight`'s retry. `printed` is
    /// what the leader had read when it was stopped.
    LeaderTimedOut {
        flight: Arc<SharedRead>,
        printed: Option<BoundedRun>,
    },
}

/// A caller that asked again after a leader's timeout ends at its own
/// deadline like a solo call: naming the budget it was given rather than the
/// remainder a rerun was handed, and, when it keeps prefixes, with the
/// longest prefix it saw, since a solo call over the same budget would have
/// read at least that much.
fn as_the_callers_deadline(
    outcome: Result<BoundedRun, CommandError>,
    prefix: Option<BoundedRun>,
    label: &str,
    timeout: Duration,
) -> Result<BoundedRun, CommandError> {
    if !is_deadline_outcome(&outcome) {
        return outcome;
    }
    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
    let longer = |own: usize| {
        prefix
            .as_ref()
            .filter(|_| keeping_deadline_prefix())
            .is_some_and(|kept| kept.stdout.len() > own)
    };
    match outcome {
        Ok(run) if !longer(run.stdout.len()) => Ok(rename_deadline(run, message)),
        Err(error) if !longer(0) => {
            // A refusal to wait any longer for a slot says so; only the bare
            // timeout is rewritten.
            Err(if error.message().ends_with(SLOT_WAIT_SUFFIX) {
                error
            } else {
                message.into()
            })
        }
        // The leader's prefix outweighs this caller's own.
        _ => match prefix {
            Some(kept) => Ok(rename_deadline(kept, message)),
            None => Err(message.into()),
        },
    }
}

fn rename_deadline(mut run: BoundedRun, message: String) -> BoundedRun {
    if let Some(Incomplete::Deadline { message: slot, .. }) = &mut run.incomplete {
        *slot = message;
    }
    run
}

/// Whether a flight's outcome is its leader's deadline rather than an answer.
fn is_deadline_outcome(outcome: &Result<BoundedRun, CommandError>) -> bool {
    match outcome {
        Ok(run) => matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
        Err(error) => is_timed_out(error.message()),
    }
}

/// What a solo call that ran out of `timeout` reports: the prefix form for a
/// caller keeping deadline prefixes, the error otherwise (see
/// [`run_admitted`]).
fn deadline_expired(label: &str, timeout: Duration) -> Result<BoundedRun, CommandError> {
    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
    if keeping_deadline_prefix() {
        return Ok(BoundedRun {
            stdout: Vec::new(),
            stderr: Vec::new(),
            success: false,
            status_code: -1,
            incomplete: Some(Incomplete::Deadline {
                message,
                over_cap: None,
            }),
            stderr_incomplete: None,
            cancelled: false,
        });
    }
    let error = CommandError::from(message);
    note_process_failure(&error);
    Err(error)
}

fn run_read_shared_once(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    deadline: Instant,
    stdout_cap: usize,
    gate: &'static SpawnGate,
    after: Option<&Arc<SharedRead>>,
) -> SharedAttempt {
    let Some(cwd) = cmd.get_current_dir().map(Path::to_path_buf) else {
        return SharedAttempt::Done(run_with_gate(
            cmd,
            label,
            timeout,
            None,
            stdout_cap,
            &mut (),
            gate,
        ));
    };
    let key = SharedReadKey {
        cwd,
        program: cmd.get_program().to_os_string(),
        argv: cmd.get_args().map(std::ffi::OsStr::to_os_string).collect(),
        stdout_cap,
        class: current_admission(),
    };
    let table = shared_reads();
    let (flight, leader) = {
        let mut reads = table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Callers whose leader timed out share one retry flight, however
        // far its child has got, so a hung read meets one new child and not
        // one per caller. The table is held, so the first to arrive decides.
        let mut retry = after.map(|previous| {
            previous
                .retry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        let known = retry
            .as_deref()
            .and_then(Clone::clone)
            .or_else(|| reads.get(&key).cloned());
        let (flight, leader) = match known {
            Some(flight) => (flight, false),
            None => {
                let flight = SharedRead::new();
                reads.insert(key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };
        if let Some(slot) = retry.as_deref_mut() {
            slot.get_or_insert_with(|| Arc::clone(&flight));
        }
        (flight, leader)
    };
    if !leader {
        gate.note_coalesced(key.class);
        let mut result = flight
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The leader always publishes, even when it panics (`Publish`), but
        // its child may outlast this caller's deadline: wait no longer than
        // that. An answer that is there wins over a deadline that has just
        // passed. There is no cancel on this path to poll: the read runs
        // without an observer, so only the deadline bounds the wait.
        let shared = loop {
            if let Some(shared) = result.as_ref() {
                break Some(shared.clone());
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break None;
            }
            result = flight
                .done
                .wait_timeout(result, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        };
        drop(result);
        let Some(shared) = shared else {
            return SharedAttempt::Done(deadline_expired(label, timeout));
        };
        if is_deadline_outcome(&shared) && Instant::now() < deadline {
            // Not answered by another's child after all.
            gate.note_uncoalesced(key.class);
            return SharedAttempt::LeaderTimedOut {
                flight: Arc::clone(&flight),
                printed: shared.ok(),
            };
        }
        // The leader counted its own failure on its thread; a joiner that
        // received it saw no answer either.
        if let Err(error) = &shared {
            note_process_failure(error);
        }
        return SharedAttempt::Done(shared);
    }

    /// Closes the table entry and publishes whatever the leader ended with,
    /// so a leader that panics or is refused cannot strand its joiners.
    struct Publish<'a> {
        key: &'a SharedReadKey,
        flight: &'a Arc<SharedRead>,
        label: &'a str,
        outcome: Option<Result<BoundedRun, CommandError>>,
    }
    impl Drop for Publish<'_> {
        fn drop(&mut self) {
            close_shared_read(self.key, self.flight);
            let outcome = self.outcome.take().unwrap_or_else(|| {
                Err(format!("{} shared read ended without a result", self.label).into())
            });
            *self
                .flight
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
            self.flight.done.notify_all();
        }
    }
    let mut publish = Publish {
        key: &key,
        flight: &flight,
        label,
        outcome: None,
    };
    let outcome = run_admitted(
        cmd,
        label,
        timeout,
        None,
        stdout_cap,
        &mut (),
        gate,
        &|| close_shared_read(&key, &flight),
    );
    publish.outcome = Some(outcome.clone());
    drop(publish);
    SharedAttempt::Done(outcome)
}

/// Removes `flight` from the table if it is still the entry for `key`; a
/// later flight under the same key is left alone.
fn close_shared_read(key: &SharedReadKey, flight: &Arc<SharedRead>) {
    let mut reads = shared_reads()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reads
        .get(key)
        .is_some_and(|entry| Arc::ptr_eq(entry, flight))
    {
        reads.remove(key);
    }
}

/// Writes the totals into the diagnostic log when the gate refuses work, at
/// most every [`REFUSAL_REPORT_INTERVAL`], so an exported log says whether a
/// failed action was a deferred refresh, shed background work or a timeout,
/// and how much was admitted meanwhile. A real log entry rather than a line
/// appended at export: the export's "empty log" verdict must stay true.
fn log_refusals(
    counters: &[AdmissionCounters; 3],
    sharing: &str,
    refused_by: Option<BucketLevel>,
    top: impl FnOnce() -> String,
) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    let now = Instant::now();
    {
        let mut last = LAST
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.is_some_and(|at| now.saturating_duration_since(at) < REFUSAL_REPORT_INTERVAL) {
            return;
        }
        *last = Some(now);
    }
    let refused_by = refused_by.map_or_else(
        || "last refusal named no bucket".to_string(),
        |level| format!("last refusal: {}", level.describe()),
    );
    log::warn!(
        target: "spawn_gate",
        "{} | {refused_by} | {}",
        format_gate_report(counters, sharing),
        top()
    );
}

/// A decision that fell back to this process's own budget, logged with the
/// running total at most every [`REFUSAL_REPORT_INTERVAL`].
fn log_shared_fallback(reason: &str, total: u64) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if !report_due(&LAST) {
        return;
    }
    log::warn!(
        target: "spawn_gate",
        "shared spawn budget unavailable for a decision ({total} so far); used this process's own: {reason}"
    );
}

/// A denial the shared record could not persist, logged at most every
/// [`REFUSAL_REPORT_INTERVAL`], with the running total. The spawn was refused
/// all the same.
fn log_unrecorded_denial(reason: &str, total: u64) {
    static LAST: Mutex<Option<Instant>> = Mutex::new(None);
    if !report_due(&LAST) {
        return;
    }
    log::warn!(
        target: "spawn_gate",
        "shared spawn budget denied a decision it could not record ({total} so far); refused: {reason}"
    );
}

/// Whether `last` is older than [`REFUSAL_REPORT_INTERVAL`]; if so, now is
/// recorded as the latest report.
fn report_due(last: &Mutex<Option<Instant>>) -> bool {
    let now = Instant::now();
    let mut last = last
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if last.is_some_and(|at| now.saturating_duration_since(at) < REFUSAL_REPORT_INTERVAL) {
        return false;
    }
    *last = Some(now);
    true
}

const REFUSAL_REPORT_INTERVAL: Duration = Duration::from_secs(30);

fn format_gate_report(counters: &[AdmissionCounters; 3], sharing: &str) -> String {
    let classes: Vec<String> = Admission::ALL
        .iter()
        .map(|class| {
            let c = counters[class.index()];
            format!(
                "{} admitted={} coalesced={} deferred={} shed={} timed_out={} cancelled={}",
                class.name(),
                c.admitted,
                c.coalesced,
                c.refused,
                c.shed,
                c.timed_out,
                c.cancelled
            )
        })
        .collect();
    format!("[spawn-gate] {} | {sharing}", classes.join(" | "))
}

/// The process-wide gate, built by the first spawn. Module-level so that
/// [`run_process_with_shared_spawn_budget`] can refuse to run after it.
static SPAWN_GATE: OnceLock<SpawnGate> = OnceLock::new();

fn spawn_gate() -> &'static SpawnGate {
    // Tests share this process-wide gate and spawn far above the storm cap.
    // Production uses the same constructor with `testing == false`.
    SPAWN_GATE.get_or_init(|| configured_spawn_gate(cfg!(test)))
}

/// Shared engine behind `git_timeout` and `capture_command`: spawns `cmd`,
/// enforces `timeout`, and bounds stdout/stderr.
///
/// `label` names the process in spawn/timeout/wait errors (callers keep their
/// own wording for truncation). When `stdin_bytes` is set, the child gets a
/// piped stdin pumped without blocking the deadline loop, even while megabytes
/// are still being pushed into the child.
pub(crate) fn run_bounded(
    cmd: Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<BoundedRun, CommandError> {
    run_bounded_capped(cmd, label, timeout, stdin_bytes, MAX_OUTPUT_BYTES)
}

/// [`run_bounded`] with an explicit stdout budget.
///
/// [`MAX_OUTPUT_BYTES`] is a backstop against a runaway process, not a sane
/// payload size: 64 MiB of diff text costs ~90 MiB in this process (the bytes
/// plus the lossy `String` copy), ~44 MiB more to serialize for IPC, and
/// ~330 MiB once the webview holds the string and its parsed rows. Callers
/// whose output lands in the UI pass the budget their surface can actually
/// render, and tell the user when they hit it.
pub(crate) fn run_bounded_capped(
    mut cmd: Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
) -> Result<BoundedRun, CommandError> {
    run_observed(&mut cmd, label, timeout, stdin_bytes, stdout_cap, &mut ())
}

pub(crate) fn run_observed(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
) -> Result<BoundedRun, CommandError> {
    run_with_gate(
        cmd,
        label,
        timeout,
        stdin_bytes,
        stdout_cap,
        observer,
        spawn_gate(),
    )
}

fn run_with_gate(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
) -> Result<BoundedRun, CommandError> {
    run_admitted(
        cmd,
        label,
        timeout,
        stdin_bytes,
        stdout_cap,
        observer,
        gate,
        &|| {},
    )
}

thread_local! {
    /// Set only by [`without_lazy_fetch`]: every git this thread builds gets
    /// `GIT_NO_LAZY_FETCH=1`.
    static NO_LAZY_FETCH: Cell<bool> = const { Cell::new(false) };
}

/// Runs `body` with every git command built on this thread told not to fetch
/// missing objects from a promisor remote (`GIT_NO_LAZY_FETCH=1`, git 2.44 and
/// later; older gits ignore an unknown variable, which is why a read that must
/// not fetch also refuses a partial clone up front instead of relying on this).
pub(crate) fn without_lazy_fetch<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            NO_LAZY_FETCH.set(self.0);
        }
    }
    let _restore = Restore(NO_LAZY_FETCH.replace(true));
    body()
}

thread_local! {
    /// Set only by [`KeepDeadlinePrefix`]. A capped git read holds it so a
    /// deadline returns the bytes already captured. Every other caller leaves
    /// it unset and still receives a timeout error.
    static KEEP_DEADLINE_PREFIX: Cell<bool> = const { Cell::new(false) };
    static PROCESS_FAILURES: Cell<u64> = const { Cell::new(0) };
    static LAST_PROCESS_FAILURE: std::cell::RefCell<Option<CommandError>> =
        const { std::cell::RefCell::new(None) };
    #[cfg(test)]
    static FORCE_SPAWN_FAILURE: Cell<bool> = const { Cell::new(false) };
    /// Every spawn on this thread is refused as the gate sheds background work.
    #[cfg(test)]
    static FORCE_DEFERRAL: Cell<bool> = const { Cell::new(false) };
    /// Runs at every forced failure, so a test can tell when a computation
    /// is in progress, or stretch it, without timing guesses.
    #[cfg(test)]
    static FORCED_FAILURE_HOOK: std::cell::RefCell<Option<Box<dyn Fn()>>> =
        const { std::cell::RefCell::new(None) };
    /// Labels of the children this thread asked the gate to run, newest last,
    /// for [`spawn_labels_during`].
    #[cfg(test)]
    static SPAWN_LABELS: std::cell::RefCell<Vec<String>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Fails only the spawns on this thread whose arguments include this one.
    #[cfg(test)]
    static FORCED_FAILURE_ARG: std::cell::RefCell<Option<String>> =
        const { std::cell::RefCell::new(None) };
}

/// Child processes on this thread that produced no answer at all: refused or
/// deferred by the gate, failed to spawn, or killed at their deadline. A
/// non-zero git exit is an answer and is not counted; neither is a run whose
/// output was cut short, which carries its own `incomplete` reason. A deadline
/// whose prefix a capped read kept is that second case: it is counted only
/// when a caller that required the whole stream rejects it.
///
/// Callers that swallow a git error as "not set" (`config --get`, a
/// `symbolic-ref --quiet`) cannot tell those apart from a real "not set".
/// Comparing this counter across a computation tells them whether it saw
/// the repository or saw the gate, so a cache never keeps an answer that
/// was really a refusal.
pub(crate) fn process_failures() -> u64 {
    PROCESS_FAILURES.get()
}

/// What the newest failure counted by [`process_failures`] said. A caller
/// that compared the counter and found a failure it swallowed can report the
/// cause instead of an empty field — and keep its kind, so the frontend knows
/// a deferred answer is worth asking for again.
pub(crate) fn last_process_failure() -> Option<CommandError> {
    LAST_PROCESS_FAILURE.with(|slot| slot.borrow().clone())
}

fn note_process_failure(error: &CommandError) {
    PROCESS_FAILURES.set(PROCESS_FAILURES.get().wrapping_add(1));
    LAST_PROCESS_FAILURE.with(|slot| *slot.borrow_mut() = Some(error.clone()));
}

/// While alive on this thread, a child killed at its runtime deadline is a
/// captured prefix ([`Incomplete::Deadline`]) rather than an error that drops
/// those bytes. [`git_run_inner`] is the only production holder: pulse and
/// the other capped git reads can show what git already printed, and a
/// command that must see the whole stream still fails.
struct KeepDeadlinePrefix;

impl KeepDeadlinePrefix {
    fn enter() -> Self {
        KEEP_DEADLINE_PREFIX.set(true);
        Self
    }
}

impl Drop for KeepDeadlinePrefix {
    fn drop(&mut self) {
        KEEP_DEADLINE_PREFIX.set(false);
    }
}

fn keeping_deadline_prefix() -> bool {
    KEEP_DEADLINE_PREFIX.get()
}

/// Every child spawned on this thread inside `body` fails as a spawn error,
/// without starting. Lets a test reproduce "the gate refused" independent of
/// machine load.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCE_SPAWN_FAILURE.set(self.0);
        }
    }
    let _restore = Restore(FORCE_SPAWN_FAILURE.replace(true));
    body()
}

/// Every child spawned on this thread inside `body` is refused exactly as the
/// gate refuses shed background work ([`Refusal::Shed`]): the error a
/// `gitpulse-mcp` read gets when the spawn budget is low, independent of
/// machine load.
#[cfg(test)]
pub(crate) fn with_forced_deferral<T>(body: impl FnOnce() -> T) -> T {
    struct Restore(bool);
    impl Drop for Restore {
        fn drop(&mut self) {
            FORCE_DEFERRAL.set(self.0);
        }
    }
    let _restore = Restore(FORCE_DEFERRAL.replace(true));
    body()
}

/// [`with_forced_spawn_failure`] for only the spawns whose arguments include
/// `arg` (a subcommand such as `"symbolic-ref"`), so a test can refuse one read
/// of a computation and let the rest answer.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure_of<T>(arg: &str, body: impl FnOnce() -> T) -> T {
    struct Restore(Option<String>);
    impl Drop for Restore {
        fn drop(&mut self) {
            let previous = self.0.take();
            FORCED_FAILURE_ARG.with(|slot| *slot.borrow_mut() = previous);
        }
    }
    let previous = FORCED_FAILURE_ARG.with(|slot| slot.borrow_mut().replace(arg.to_string()));
    let _restore = Restore(previous);
    body()
}

/// True while this thread is inside [`with_forced_spawn_failure`] or
/// [`with_forced_spawn_failure_of`]. Process-wide caches of spawn results
/// consult it: the forced refusal is thread-local, the cache is shared, and
/// another test warming the cache would otherwise let the real tool answer past
/// the refusal.
#[cfg(test)]
pub(crate) fn forced_failure_active() -> bool {
    FORCE_SPAWN_FAILURE.get() || FORCED_FAILURE_ARG.with(|slot| slot.borrow().is_some())
}

#[cfg(test)]
fn forced_failure_matches(cmd: &Command) -> bool {
    FORCED_FAILURE_ARG.with(|slot| {
        slot.borrow()
            .as_deref()
            .is_some_and(|wanted| cmd.get_args().any(|arg| arg == wanted))
    })
}

/// [`with_forced_spawn_failure`], calling `hook` at each forced failure.
#[cfg(test)]
pub(crate) fn with_forced_spawn_failure_then<T>(
    hook: impl Fn() + 'static,
    body: impl FnOnce() -> T,
) -> T {
    struct Clear;
    impl Drop for Clear {
        fn drop(&mut self) {
            FORCED_FAILURE_HOOK.with(|slot| slot.borrow_mut().take());
        }
    }
    FORCED_FAILURE_HOOK.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    let _clear = Clear;
    with_forced_spawn_failure(body)
}

/// Runs `body` and returns the label of every child it asked this thread's
/// gate to spawn — the spawn-count assertion for "one process, not N". Only
/// spawns made on the calling thread are seen, which is where a synchronous
/// reader makes them.
#[cfg(test)]
pub(crate) fn spawn_labels_during<R>(body: impl FnOnce() -> R) -> (R, Vec<String>) {
    SPAWN_LABELS.with(|labels| labels.borrow_mut().clear());
    let result = body();
    let seen = SPAWN_LABELS.with(|labels| std::mem::take(&mut *labels.borrow_mut()));
    (result, seen)
}

/// Asks `child` to stop and spends the grace reading its pipes.
///
/// A child told to stop writes while it cleans up, and one blocked on a full
/// pipe cannot finish, so a sleeping grace gets it SIGKILLed with its
/// `.git/index.lock` still held. The wait is the output poll itself, so the
/// grace both waits and drains, and procguard still ends it at [`CHILD_GRACE`]
/// however much arrives; the bytes land in the same capped buffers. Stdin is
/// closed once the ask has been sent and not fed again: a cleanup that reads
/// stdin to EOF must see it, but never before the ask. And `input.finish()` afterwards still only turns undelivered
/// bytes into an error for a successful, uncancelled run. A panic inside the
/// wait is handled by `Registration::drop`, which force-kills a child no reap
/// path has claimed. Where the platform cannot ask there is no grace, and on
/// non-Unix the pipe workers already read on their own threads.
fn ask_and_drain(
    guard: &crate::procguard::Registration,
    child: &mut std::process::Child,
    #[cfg(unix)] output: &mut pipe_drain::OutputDrains,
    #[cfg(not(unix))] _output: &mut thread_io::OutputDrains,
    #[cfg(unix)] input: &mut pipe_drain::InputFeed<'_>,
    #[cfg(not(unix))] _input: &mut thread_io::InputFeed,
) -> std::io::Result<std::process::ExitStatus> {
    #[cfg(unix)]
    return guard.ask_then_stop_and_reap_waiting(child, &mut |span| {
        // First wait: SIGTERM has already been sent, so a child that reads
        // EOF now was asked to stop first and cannot take truncated input
        // for the whole of it.
        input.close();
        output.drain_ready();
        if output.wait_with_input(span, None).is_err() {
            std::thread::sleep(span);
        }
        output.drain_ready();
    });
    #[cfg(not(unix))]
    guard.ask_then_stop_and_reap(child)
}

/// [`run_with_gate`] with `on_admitted` called after the gate admits the
/// child and before it is spawned — the last moment at which nothing has been
/// read yet, which is what [`run_read_shared`] needs to stop new joiners.
#[allow(clippy::too_many_arguments)]
fn run_admitted(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
    on_admitted: &dyn Fn(),
) -> Result<BoundedRun, CommandError> {
    #[cfg(test)]
    SPAWN_LABELS.with(|labels| labels.borrow_mut().push(label.to_string()));
    #[cfg(test)]
    if FORCE_DEFERRAL.get() {
        return Err(refusal_message(label, Refusal::Shed));
    }
    #[cfg(test)]
    if FORCE_SPAWN_FAILURE.get() || forced_failure_matches(cmd) {
        FORCED_FAILURE_HOOK.with(|slot| {
            if let Some(hook) = slot.borrow().as_ref() {
                hook();
            }
        });
        let error = CommandError::from(format!("Failed to spawn {label}: forced by test"));
        note_process_failure(&error);
        return Err(error);
    }
    let result = run_admitted_inner(
        cmd,
        label,
        timeout,
        stdin_bytes,
        stdout_cap,
        observer,
        gate,
        on_admitted,
    );
    // A capped git read asked to keep the prefix. Everyone else still sees
    // the deadline as an error, with the same sentence as before.
    let result = match result {
        Ok(run)
            if !keeping_deadline_prefix()
                && matches!(run.incomplete, Some(Incomplete::Deadline { .. })) =>
        {
            let Some(Incomplete::Deadline { message, .. }) = run.incomplete else {
                unreachable!("matched Deadline above");
            };
            Err(message.into())
        }
        other => other,
    };
    if let Err(error) = &result {
        note_process_failure(error);
    }
    result
}

#[allow(clippy::too_many_arguments)]
fn run_admitted_inner(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    observer: &mut dyn ProcessObserver,
    gate: &'static SpawnGate,
    on_admitted: &dyn Fn(),
) -> Result<BoundedRun, CommandError> {
    if timeout > NETWORK_TIMEOUT {
        return Err(format!("{label} deadline exceeds {}s", NETWORK_TIMEOUT.as_secs()).into());
    }
    if timeout.is_zero() {
        return Err(format!(
            "{label}{TIMEOUT_MARKER}{}s: deadline must be positive and at most {}s",
            timeout.as_secs_f64(),
            NETWORK_TIMEOUT.as_secs()
        )
        .into());
    }
    if stdout_cap > MAX_OUTPUT_BYTES
        || stdin_bytes.is_some_and(|bytes| bytes.len() > MAX_OUTPUT_BYTES)
    {
        return Err(format!(
            "{label} input/output budget exceeds the {MAX_OUTPUT_BYTES} byte limit"
        )
        .into());
    }
    let queue_deadline = Instant::now() + timeout;
    if stdin_bytes.is_some() {
        cmd.stdin(Stdio::piped());
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.stdout(Stdio::piped()).stderr(Stdio::piped());
    // Sited here rather than at each construction site because this is the one
    // funnel every spawn in the process already passes through: `git_timeout`,
    // `capture_command`, `run_bounded`, `run_bounded_capped` and `run_observed`
    // all end up in this function. A per-site fix would have to be remembered
    // by the next spawn someone adds; this one cannot be forgotten.
    default_child_path(cmd);

    // Held until this function returns, covering the child's descriptors, its
    // output buffers and fallback reader threads -- see [`SpawnGate`] for why an
    // unbounded fan-out here exhausted the process descriptor table.
    // A refresh in the repository a user action just changed is admitted
    // like the action itself, within the credit `run_command_scope` granted.
    let credited = current_admission() == Admission::Reactive
        && cmd
            .get_current_dir()
            .is_some_and(|dir| take_post_action_credit(dir, Instant::now()));
    let class = if credited {
        Admission::Interactive
    } else {
        current_admission()
    };
    let _permit = Arc::new(
        with_admission(class, || {
            gate.acquire_judged(queue_deadline, &|| observer.cancelled())
        })
        .map_err(|(refusal, level)| refusal_message_at(label, refusal, level))?,
    );
    gate.note_admitted_label(label);
    on_admitted();

    // Slot wait is already bounded by `queue_deadline`. The child then gets
    // the full requested runtime: a 5s git call that spent 4.9s queued must
    // not be reported as "timed out" after 100ms of actual work. Under
    // llvm-cov that is how shebang and stub-devmap tests failed a 5s/30s
    // deadline they would have met once they started.
    let deadline = Instant::now() + timeout;

    // Spawned into a process group of its own and registered, so a SIGTERM to
    // this process — or the deliberate `process::exit` on `gitpulse-mcp`'s
    // shutdown path — takes this child and everything it forked down with it
    // instead of orphaning them. See [`crate::procguard`].
    //
    // A background-band child enters the band itself, before it execs: applied
    // from here after the spawn, the band missed any child that exec'd at once
    // (every `sh -c`, and the `git` shims), and a child that cannot enter it
    // fails the spawn rather than silently running at normal priority. See
    // [`crate::procguard::spawn_background`] for the mechanism and its cost.
    let in_band = runs_in_background_band(class);
    let spawned = if in_band {
        crate::procguard::spawn_background(cmd, label)
    } else {
        crate::procguard::spawn(cmd, label)
    };
    #[cfg(all(test, target_os = "macos"))]
    if in_band {
        count_band_spawn(&spawned);
    }
    let (mut child, guard) = spawned.map_err(|e| {
        let message = spawn_error(label, &e);
        // A trust refusal keeps its kind in other words, which is what a
        // caller that only holds the spawn error asks about.
        match crate::procguard::trust_refusal(&e) {
            Some(refusal) => refusal.clone().wrap(|_| message),
            None => message.into(),
        }
    })?;
    #[cfg(test)]
    spawn_log::record(cmd);

    // On Unix the command waiter owns and drains both nonblocking pipes.
    // EOF no longer depends on two separately scheduled reader threads
    // handing their entire result over inside a two-second window.
    #[cfg(unix)]
    let mut output =
        match pipe_drain::OutputDrains::new(child.stdout.take(), child.stderr.take(), stdout_cap) {
            Ok(output) => output,
            Err(error) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                return Err(format!("Failed to prepare {label} output: {error}").into());
            }
        };
    #[cfg(not(unix))]
    let mut output = match thread_io::OutputDrains::new(
        child.stdout.take(),
        child.stderr.take(),
        stdout_cap,
        _permit.clone(),
    ) {
        Ok(output) => output,
        Err(error) => {
            guard.stop_and_reap(&mut child, Duration::ZERO).ok();
            return Err(format!("Failed to prepare {label} output: {error}"));
        }
    };

    // Input delivery is bounded independently of child exit. A successful
    // exit cannot conceal rejected or undelivered stdin.
    #[cfg(unix)]
    let mut input =
        match pipe_drain::InputFeed::new(child.stdin.take(), stdin_bytes.unwrap_or_default()) {
            Ok(input) => input,
            Err(error) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                return Err(format!("Failed to prepare {label} stdin: {error}").into());
            }
        };
    #[cfg(not(unix))]
    let mut input = match thread_io::InputFeed::new(
        child.stdin.take(),
        stdin_bytes.unwrap_or_default(),
        _permit.clone(),
    ) {
        Ok(input) => input,
        Err(error) => {
            guard.stop_and_reap(&mut child, Duration::ZERO).ok();
            return Err(format!("Failed to prepare {label} stdin: {error}"));
        }
    };

    let mut backoff = POLL_BACKOFF_START;
    let mut cursors = [0; 2];
    enum Halt {
        Exited(std::process::ExitStatus, bool),
        /// Runtime deadline. The child has been reaped. `message` is the
        /// timeout sentence callers that cannot keep a prefix still return.
        Deadline(String),
        Failed(String),
    }
    let halt = loop {
        #[cfg(unix)]
        output.drain_ready();
        #[cfg(unix)]
        input.pump();
        output.observe(observer, &mut cursors);
        if observer.cancelled() {
            // Asked first: git removes its own `.git/index.lock` when asked and
            // cannot when killed, so a cancelled commit would block the next.
            break match ask_and_drain(&guard, &mut child, &mut output, &mut input) {
                Ok(status) => Halt::Exited(status, true),
                Err(e) => Halt::Failed(format!("Failed to reap cancelled {label}: {e}")),
            };
        }
        // Every wait goes through the registration so the registry never
        // holds a pid that has already been reaped — see `procguard` for why
        // signalling a recycled pid is the thing to avoid.
        match guard.poll(|| child.try_wait()) {
            Ok(Some(status)) => break Halt::Exited(status, false),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
                    // Reap before reading the rest of the pipes: a live child
                    // can hold them open. The ask comes first so git can
                    // release `.git/index.lock`; a child that ignores it is
                    // killed after [`CHILD_GRACE`], which a child that exits
                    // sooner does not wait out.
                    ask_and_drain(&guard, &mut child, &mut output, &mut input).ok();
                    break Halt::Deadline(message);
                }
                #[cfg(unix)]
                if let Err(error) = output.wait_with_input(
                    backoff.min(deadline.saturating_duration_since(Instant::now())),
                    Some(&input),
                ) {
                    guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                    break Halt::Failed(format!("Failed to poll {label} output: {error}"));
                }
                #[cfg(not(unix))]
                thread::sleep(backoff);
                backoff = next_poll_backoff(backoff);
            }
            Err(e) => {
                guard.stop_and_reap(&mut child, Duration::ZERO).ok();
                break Halt::Failed(format!("Failed to wait on {}: {}", label, e));
            }
        }
    };
    let (status, cancelled, deadline_message) = match halt {
        Halt::Failed(error) => return Err(error.into()),
        Halt::Exited(status, cancelled) => (Some(status), cancelled, None),
        Halt::Deadline(message) => (None, false, Some(message)),
    };
    if let Err(error) = input.finish() {
        if deadline_message.is_none() && status.is_some_and(|status| status.success()) && !cancelled
        {
            return Err(format!("Failed to deliver {label} {error}").into());
        }
    }
    // Normal exit: EOF is imminent unless something else is holding a
    // write end; then we take whatever was buffered after the grace
    // window instead of hanging forever. Two things can hold one, and
    // the rarer-sounding one is what actually fired in the field: a
    // grandchild the child daemonized, and — until
    // `procguard::with_inheritance_lock` — any sibling spawned inside
    // this pipe's pre-`FD_CLOEXEC` window, which on macOS `std` cannot
    // close atomically. Do not read this reason as the first cause
    // alone. A deadline already reaped the child, so there is no grace
    // left to spend: the bytes in the buffer are the prefix.
    let (mut stdout, mut stderr) = output.finish(
        Instant::now()
            + if cancelled || deadline_message.is_some() {
                Duration::ZERO
            } else {
                DRAIN_JOIN_GRACE
            },
    );
    observe_output(observer, &mut cursors, &stdout.bytes, &stderr.bytes);
    // A stdout we could not read to the end is not a shorter stdout:
    // every caller past this point parses what it is handed as the
    // whole answer. A broken read is a fault and fails the run; an
    // undelivered one is the documented grandchild case, where the
    // child's status is still good — that reports as a prefix, and
    // carries its reason so no caller has to guess at one.
    // A deadline is itself the reason the read stopped. A pipe that
    // breaks because the child was killed must not discard the prefix.
    let mut unread: Option<String> = None;
    match stdout.stop.take() {
        Some(Stop::Broken(e)) if deadline_message.is_none() => {
            return Err(format!("Failed to read {label} output: {e}").into());
        }
        Some(Stop::Broken(e) | Stop::Undelivered(e)) => {
            stderr
                .bytes
                .extend_from_slice(format!("\n[stdout incomplete: {e}]").as_bytes());
            if deadline_message.is_none() {
                unread = Some(e);
            }
        }
        None => {}
    }
    // stderr is diagnosis, not payload: losing it must not fail a
    // command that worked, but a message built from a partial stderr
    // has to say that is what it is.
    let mut stderr_incomplete = None;
    if let Some(Stop::Broken(e) | Stop::Undelivered(e)) = stderr.stop.take() {
        stderr
            .bytes
            .extend_from_slice(format!("\n[stderr incomplete: {e}]").as_bytes());
        stderr_incomplete = Some(Incomplete::Unread(e));
    }
    if stderr.truncated {
        stderr_incomplete = Some(Incomplete::OverCap(4 * 1024 * 1024));
        stderr
            .bytes
            .extend_from_slice(b"\n[stderr incomplete: exceeded 4 MB]");
    }
    // A capped stream may also miss EOF. The proven cap violation is
    // reported first; the read diagnosis remains in stderr. A deadline
    // keeps the cap on the same variant so the prefix is not thrown
    // away on the way out of this function.
    let incomplete = if let Some(message) = deadline_message.clone() {
        Some(Incomplete::Deadline {
            message,
            over_cap: stdout.truncated.then_some(stdout_cap),
        })
    } else if stdout.truncated {
        Some(Incomplete::OverCap(stdout_cap))
    } else {
        unread.map(Incomplete::Unread)
    };
    Ok(BoundedRun {
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        success: deadline_message.is_none()
            && status.is_some_and(|status| status.success())
            && !cancelled,
        status_code: status.and_then(|status| status.code()).unwrap_or(-1),
        incomplete,
        stderr_incomplete,
        cancelled,
    })
}

/// Characterizes the retired channel handoff in regression tests. Production
/// owns captured prefixes independently of worker completion on every platform.
#[cfg(test)]
fn collect_drained_deadline(rx: &mpsc::Receiver<Drained>, deadline: Instant) -> Drained {
    let remaining = deadline.saturating_duration_since(Instant::now());
    rx.recv_timeout(remaining).unwrap_or_else(|e| Drained {
        // Emphatically not `Default`: an empty `Vec` with no stop reason says
        // "the child produced nothing", which is a real and common answer. A
        // drain that never delivered produced *unknown*, and the two must not
        // arrive as the same value.
        stop: Some(Stop::Undelivered(format!(
            "output reader did not finish: {e}"
        ))),
        ..Drained::default()
    })
}

/// Detects transient git lock contention (a background agent or terminal
/// holding `index.lock`) from git's OWN diagnosis: the first line of the
/// message, which is where git states a lock it could not take.
///
/// Only the first line is read. Anything later is output git relayed, and a
/// pre-commit hook or a push's `remote:` lines can quote a lock error that
/// belongs to some other process. A first line that is itself a relayed
/// `remote:` line is not git's. The permanent ref-lock forms carry no
/// `Unable to create ...lock` on the first line and never match:
/// `'refs/heads/a' exists; cannot create ...` (a directory/file conflict) and
/// `is at X but expected Y` (a lost race that retrying cannot win).
///
/// The first line is searched, not anchored, because a caller may prefix its
/// own wrapper (`Index update failed for the N selected paths: fatal: ...`).
/// Whether the command is safe to run again at all is
/// [`lock_retry_allowed`]'s question, not this one's.
pub fn is_transient_git_lock_error(err: &str) -> bool {
    // The quoted lock path is blanked before the guard phrases are looked for,
    // so a repository path that spells `remote:` or `but expected` is still a
    // lock, and the guards match only git's own message structure.
    let first = err.lines().next().unwrap_or_default().to_ascii_lowercase();
    let Some(lock) = lock_path_span(&first) else {
        return first.contains("another git process seems to be running")
            && !first.trim_start().starts_with("remote:");
    };
    let rest = format!("{}{}", &first[..lock.start], &first[lock.end..]);
    let lost_race = rest
        .find(" is at ")
        .is_some_and(|at| rest[at..].contains(" but expected "));
    if rest.trim_start().starts_with("remote:")
        || rest.contains("exists; cannot create")
        || lost_race
    {
        return false;
    }
    lock.directory_create || rest.contains("file exists")
}

/// The quoted `...lock` path in git's "could not take this lock" wording, by
/// byte offsets into the lowercased first line.
struct LockPath {
    start: usize,
    end: usize,
    /// `unable to create directory for '...lock'`: a racing ref-directory
    /// create, which says no `File exists`.
    directory_create: bool,
}

fn lock_path_span(first_lower: &str) -> Option<LockPath> {
    for (opener, directory_create) in [
        ("unable to create directory for '", true),
        ("unable to create '", false),
    ] {
        if let Some(at) = first_lower.find(opener) {
            let start = at + opener.len();
            let end = first_lower.rfind(".lock'")? + ".lock".len();
            if end >= start {
                return Some(LockPath {
                    start,
                    end,
                    directory_create,
                });
            }
        }
    }
    None
}

/// The lock path git names on the first line of `diagnosis`, original case.
fn lock_path_named(diagnosis: &str) -> Option<&str> {
    let first = diagnosis.lines().next()?;
    let span = lock_path_span(&first.to_ascii_lowercase())?;
    first.get(span.start..span.end)
}

/// Whether a failed hook-running command (`commit`, `merge`, `cherry-pick`, ...)
/// failed on a lock inside THIS repository's git directory (or its common
/// directory, where ref locks live).
///
/// git takes `index.lock` before it runs any hook, so git's own lock failure
/// on this repository proves no hook started and replaying runs nothing twice.
/// A lock line that names another path is something else's: a nested git a hook
/// ran, or another repository. Anything that cannot be resolved is refused.
fn lock_is_in_this_repository(repo: &Path, diagnosis: &str) -> bool {
    let Some(named) = lock_path_named(diagnosis) else {
        return false;
    };
    let Ok(dirs) = git_text(
        repo,
        &["rev-parse", "--absolute-git-dir", "--git-common-dir"],
    ) else {
        return false;
    };
    let named = Path::new(named);
    // A lock file that is gone by now is compared through its directory.
    let named = named.canonicalize().ok().or_else(|| {
        let parent = named.parent()?.canonicalize().ok()?;
        Some(parent.join(named.file_name()?))
    });
    let Some(named) = named else {
        return false;
    };
    dirs.lines().any(|dir| {
        let dir = Path::new(dir.trim());
        let dir = if dir.is_absolute() {
            dir.to_path_buf()
        } else {
            repo.join(dir)
        };
        dir.canonicalize().is_ok_and(|dir| named.starts_with(dir))
    })
}

/// Subcommands that talk to a remote. A failure that mentions a lock may be the
/// server's, and running one again repeats the network work.
const NETWORK_SUBCOMMANDS: &[&str] = &["push", "fetch", "pull", "clone", "ls-remote"];

/// Whether `git <sub>` may be run again after ANY lock-shaped failure: never a
/// command that ran hooks (a hook can take minutes and has side effects) or
/// touched the network (see [`NETWORK_SUBCOMMANDS`]). A hook-running command
/// is replayed only on proof the hook never started: see
/// [`lock_is_in_this_repository`].
fn lock_retry_allowed(sub: &str) -> bool {
    hooks::hooks_run_by(sub).is_none() && !NETWORK_SUBCOMMANDS.contains(&sub)
}

/// The full retry decision for one failed attempt of `git <sub>`.
fn lock_retry_wanted(repo: Option<&Path>, sub: &str, diagnosis: &str) -> bool {
    if !is_transient_git_lock_error(diagnosis) || NETWORK_SUBCOMMANDS.contains(&sub) {
        return false;
    }
    lock_retry_allowed(sub)
        || (consists_only_of_gits_index_lock_failure(diagnosis)
            && repo.is_some_and(|repo| lock_is_in_this_repository(repo, diagnosis)))
}

/// The advice git prints under a lock failure, lowercased, one entry per line.
/// Current git (2.54 and 2.56, captured from real runs) prints the first line
/// alone; older git printed all five. Matched against a fixed table, never by
/// substring, so anything else on the line is not git's advice.
const LOCK_ADVICE_LINES: &[&str] = &[
    "another git process seems to be running in this repository, or the lock file may be stale",
    "another git process seems to be running in this repository, e.g.",
    "an editor opened by 'git commit'. please make sure all processes",
    "are terminated then try again. if it still fails, a git process",
    "may have crashed in this repository earlier:",
    "remove the file manually to continue.",
];

/// The lines git's own commands print after that advice when the index could
/// not be taken, captured from real `merge`, `cherry-pick`, `revert` and
/// `rebase` runs on git 2.54 and 2.56.
const LOCK_FOLLOWUP_LINES: &[&str] = &[
    "fatal: unable to write index.",
    "fatal: cherry-pick failed",
    "fatal: revert failed",
    "error: could not detach head",
];

/// True when `diagnosis` is git's own failure to take `index.lock` and nothing
/// else: line 1 is `fatal: Unable to create '<...>/index.lock': File exists.`,
/// then only git's advice block, at least one line of it.
///
/// git takes `index.lock` before it runs any hook, so when its own attempt
/// failed no hook ran and the whole stderr is this message. A hook that prints
/// a lock-shaped line of its own (a nested git, a lint-staged step) is followed
/// or preceded by other output, or lacks the advice block; either way it ran
/// something, and the command is not replayed. Only `index.lock` qualifies: a
/// ref lock on `commit` is taken after the hooks have run.
fn consists_only_of_gits_index_lock_failure(diagnosis: &str) -> bool {
    let mut lines = diagnosis.lines();
    let Some(first) = lines.next() else {
        return false;
    };
    let is_index_lock = lock_path_named(first)
        .and_then(|path| Path::new(path).file_name())
        .is_some_and(|name| name == "index.lock");
    let first_lower = first.trim_end().to_ascii_lowercase();
    let own_prefix = first_lower.starts_with("fatal: unable to create '")
        || first_lower.starts_with("error: unable to create '");
    if !is_index_lock || !own_prefix || !first_lower.ends_with(".lock': file exists.") {
        return false;
    }
    let mut advice = 0;
    for line in lines.map(|line| line.trim().to_ascii_lowercase()) {
        if line.is_empty() {
            continue;
        }
        if LOCK_ADVICE_LINES.contains(&line.as_str()) {
            advice += 1;
        } else if !LOCK_FOLLOWUP_LINES.contains(&line.as_str()) {
            return false;
        }
    }
    advice > 0
}

/// Backoff for attempts 1..=3: 50 + 150 + 300 ms = 500 ms, plus a tiny pid jitter
/// so concurrent waiters do not stampede the lock file together.
fn lock_retry_backoff_ms(attempt: usize) -> u64 {
    const STEPS: [u64; 3] = [50, 150, 300];
    let base = STEPS[(attempt.saturating_sub(1)).min(2)];
    base + (std::process::id() as u64 % 20)
}

/// Shared bounded-invocation loop behind [`git_timeout`] and
/// [`git_text_partial`]: spawns git with the lock-retry backoff and returns
/// its stdout plus whether stdout hit [`MAX_OUTPUT_BYTES`] and was cut.
/// A run that FAILED never flows through the data path — partial output
/// from a failed command is not trustworthy — so failures surface git's
/// own diagnosis (stderr first, then stdout, then the bare status) exactly
/// as before; only successful runs carry the truncation flag.
fn git_run(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<(Vec<u8>, Option<Incomplete>), CommandError> {
    git_run_capped(repo, args, timeout, stdin_bytes, MAX_OUTPUT_BYTES)
}

fn git_run_capped(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
) -> Result<(Vec<u8>, Option<Incomplete>), CommandError> {
    git_run_inner(repo, args, timeout, stdin_bytes, stdout_cap, false)
}

/// [`git_text`] for a read-only command on a refresh path: identical reads
/// still queued at the spawn gate share one child (see [`run_read_shared`]).
/// Never for a command that can write — refs, index, config or worktree.
pub(crate) fn git_text_shared(repo: &Path, args: &[&str]) -> Result<String, CommandError> {
    let sub = subcommand(args);
    let (bytes, incomplete) = git_run_inner(
        Some(repo),
        args,
        DEFAULT_TIMEOUT,
        None,
        MAX_OUTPUT_BYTES,
        true,
    )?;
    if let Some(reason) = incomplete {
        return Err(incomplete_is_failure(sub, reason).into());
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// [`git_text_capped`] for a read that identical callers may share, under the
/// same rule as [`git_text_shared`]: joined only while the leader is queued.
/// The cap is part of the sharing key, so a caller never receives a stream
/// cut at someone else's budget.
pub(crate) fn git_text_capped_shared(
    repo: &Path,
    args: &[&str],
    cap: usize,
) -> Result<(String, Option<Incomplete>), CommandError> {
    let (bytes, incomplete) = git_run_inner(Some(repo), args, DEFAULT_TIMEOUT, None, cap, true)?;
    Ok((String::from_utf8_lossy(&bytes).into_owned(), incomplete))
}

fn git_run_inner(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
    stdout_cap: usize,
    shared: bool,
) -> Result<(Vec<u8>, Option<Incomplete>), CommandError> {
    if let Some(repo) = repo {
        crate::repository_trust::require(repo)?;
    }
    let sub = subcommand(args);
    let label = format!("git {}", sub);
    let started = Instant::now();
    let mut attempts = 0;
    const MAX_LOCK_RETRIES: usize = 3;
    // A command that runs hooks can be cancelled from the UI while it lives,
    // and its timeout or cancel names the hooks instead of blaming git.
    let hooked = repo
        .filter(|_| hooks::hooks_run_by(sub).is_some())
        .map(|repo| (repo, hooks::HookedRun::enter(repo)));

    // A deadline keeps the bytes git already printed. Callers that need the
    // whole stream (`git_timeout`, `git_text_shared`) still turn that prefix
    // into an error. Callers that opted into a cap (pulse) show it.
    let _keep_deadline_prefix = KeepDeadlinePrefix::enter();
    loop {
        let cmd = git_command(repo, args);
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64()).into());
        }
        // A hook-running command is cancellable and can write, so it is
        // never shared whatever the caller asked: the shared path has no
        // observer to poll.
        let mut out = if shared && stdin_bytes.is_none() && hooked.is_none() {
            run_read_shared(cmd, &label, remaining, stdout_cap, spawn_gate())?
        } else if let Some((_, run)) = &hooked {
            let mut cmd = cmd;
            let mut observer = HookObserver(run);
            run_observed(
                &mut cmd,
                &label,
                remaining,
                stdin_bytes,
                stdout_cap,
                &mut observer,
            )?
        } else {
            run_bounded_capped(cmd, &label, remaining, stdin_bytes, stdout_cap)?
        };
        if let Some((repo, _)) = &hooked {
            if out.cancelled {
                return Err(hooks::cancelled_message(repo, sub).into());
            }
            if let Some(Incomplete::Deadline { message, .. }) = &mut out.incomplete {
                *message = hooks::timeout_message(repo, sub, message);
            }
        }
        if out.success || matches!(out.incomplete, Some(Incomplete::Deadline { .. })) {
            return Ok((out.stdout, out.incomplete));
        }
        // Some git failures report entirely on stdout — notably `commit`'s
        // "nothing added to commit" (exit 1, empty stderr). A bare status code
        // hides the one string callers match on to retry; surface the diagnosis.
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        let diagnosis = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if !diagnosis.is_empty() {
            if attempts < MAX_LOCK_RETRIES && lock_retry_wanted(repo, sub, diagnosis) {
                attempts += 1;
                std::thread::sleep(
                    Duration::from_millis(lock_retry_backoff_ms(attempts))
                        .min(timeout.saturating_sub(started.elapsed())),
                );
                continue;
            }
            return Err(clamp_failure_message(diagnosis, sub).into());
        }
        return Err(format!("git {} failed with status {}", sub, out.status_code).into());
    }
}

/// [`hooks::HookedRun`] borrowed as the observer of one attempt; the run
/// itself outlives the lock-retry loop so a cancel between attempts holds.
struct HookObserver<'a>(&'a hooks::HookedRun);

impl ProcessObserver for HookObserver<'_> {
    fn cancelled(&self) -> bool {
        self.0.cancelled()
    }
}

fn git_timeout(
    repo: Option<&Path>,
    args: &[&str],
    timeout: Duration,
    stdin_bytes: Option<&[u8]>,
) -> Result<Vec<u8>, CommandError> {
    let sub = subcommand(args);
    let (stdout, incomplete) = git_run(repo, args, timeout, stdin_bytes)?;
    if let Some(reason) = incomplete {
        return Err(incomplete_is_failure(sub, reason).into());
    }
    Ok(stdout)
}

/// A capped read may return a prefix. A caller that required the whole stream
/// fails it here. A deadline with no byte-cap is the same failure the runner
/// used to return directly, and it still moves [`process_failures`]: callers
/// that swallow `git_text` with `.ok()` tell a refusal from "not set" by that
/// counter. A prefix that filled its byte budget is an answer with a reason,
/// same as before this path existed, so it does not.
fn incomplete_is_failure(sub: &str, reason: Incomplete) -> String {
    match reason {
        Incomplete::Deadline {
            message,
            over_cap: None,
        } => {
            note_process_failure(&CommandError::from(message.as_str()));
            message
        }
        other => format!("git {sub} output {}", other.describe()),
    }
}

/// Upper bound on either stream embedded in a failure message. A chatty failure never
/// drags megabytes into an error string.
const MAX_FAILURE_MESSAGE_BYTES: usize = 2_000;

/// Share of [`MAX_FAILURE_MESSAGE_BYTES`] given to the head of a clamped
/// failure; the tail gets the rest. A hook or command opens with routine
/// chatter and *ends* with the reason it failed.
const FAILURE_HEAD_SHARE_PERCENT: usize = 35;

/// Bounds a failure diagnosis to [`MAX_FAILURE_MESSAGE_BYTES`], keeping both
/// ends on UTF-8 boundaries and announcing what was dropped.
///
/// Head-only truncation kept the wrong half: a pre-push hook running CI
/// printed its command banners and test-runner warnings first, so the 2 KB
/// head held only those and the failing check at the end was discarded.
/// The frontend's `clampMessage` keeps both ends for the same reason.
fn clamp_failure_message(diagnosis: &str, sub: &str) -> String {
    if diagnosis.len() <= MAX_FAILURE_MESSAGE_BYTES {
        return diagnosis.to_owned();
    }
    let head_budget = MAX_FAILURE_MESSAGE_BYTES * FAILURE_HEAD_SHARE_PERCENT / 100;
    let tail_budget = MAX_FAILURE_MESSAGE_BYTES - head_budget;
    let mut head_end = head_budget;
    while !diagnosis.is_char_boundary(head_end) {
        head_end -= 1;
    }
    let mut tail_start = diagnosis.len() - tail_budget;
    while !diagnosis.is_char_boundary(tail_start) {
        tail_start += 1;
    }
    format!(
        "{}\n… {} bytes elided (git {sub} output truncated) …\n{}",
        &diagnosis[..head_end],
        tail_start - head_end,
        &diagnosis[tail_start..],
    )
}

#[cfg(test)]
fn drain_capped<R: Read>(pipe: Option<R>, max_bytes: usize) -> Drained {
    let mut out = Drained::default();
    let Some(mut pipe) = pipe else {
        return out;
    };
    let mut tmp = [0u8; 16_384];
    loop {
        match pipe.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => out.append(&tmp[..n], max_bytes),
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // Not end-of-stream. Breaking here and handing back the prefix as
            // if the child had simply said less is how a half-read `git show`
            // reached a parser as a complete record and came back as "failed
            // to parse commit metadata format" — a read that broke reported as
            // a read that finished.
            Err(e) => {
                out.stop = Some(Stop::Broken(e.to_string()));
                break;
            }
        }
    }
    out
}

pub fn upstream_is_gone(track: &str) -> bool {
    track.contains("gone")
}

/// Parses a leading run of ASCII digits, saturating instead of falling back
/// to zero.
///
/// The shape this replaces is `raw.parse::<usize>().unwrap_or(0)`, which
/// collapses two very different readings into one: text that is not a number,
/// and a number too large for the type. The second is the dangerous one — a
/// count that overflows reads as *nothing happened*, which no caller can tell
/// apart from a genuinely empty result. `git diff --shortstat` had exactly
/// this bug, and these were its siblings.
///
/// Returns `None` when there are no digits at all, so callers keep whatever
/// zero-ish meaning that case already had — `git diff --numstat` writes `-`
/// for a binary file, and there zero really is the right answer.
pub fn parse_count_saturating(text: &str) -> Option<usize> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(digits.parse::<usize>().unwrap_or(usize::MAX))
}

/// [`parse_count_saturating`] for the 64-bit counters `git count-objects -v`
/// reports.
pub fn parse_u64_saturating(text: &str) -> Option<u64> {
    let digits: String = text
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    if digits.is_empty() {
        return None;
    }
    Some(digits.parse::<u64>().unwrap_or(u64::MAX))
}

/// Parses `git rev-list --left-right --count A...B` (`behind\\tahead`).
pub fn parse_left_right_count(raw: &str) -> (usize, usize) {
    let line = raw.trim();
    let mut parts = line.split(['\t', ' ']).filter(|s| !s.is_empty());
    // Saturating: an ahead/behind count too large for `usize` reading as 0
    // renders a wildly diverged branch as "in sync".
    let left = parts.next().and_then(parse_count_saturating).unwrap_or(0);
    let right = parts.next().and_then(parse_count_saturating).unwrap_or(0);
    (left, right)
}

pub fn parse_ahead_behind(track: &str) -> (usize, usize) {
    fn digits_after(haystack: &str, marker: &str) -> usize {
        let Some(idx) = haystack.find(marker) else {
            return 0;
        };
        let rest = &haystack.as_bytes()[idx + marker.len()..];
        let mut n = 0usize;
        for &b in rest {
            if b.is_ascii_digit() {
                n = n.saturating_mul(10).saturating_add((b - b'0') as usize);
            } else {
                break;
            }
        }
        n
    }
    (
        digits_after(track, "ahead "),
        digits_after(track, "behind "),
    )
}

/// The directory name a clone of `url` should land in.
///
/// The result is joined onto a caller-chosen destination, so it must be a
/// single path component and nothing else. Splitting on `/` alone was not
/// enough: a local Windows source path (`C:\src\repo` -- an ordinary thing to
/// clone from) contains no forward slash, so the whole path survived as the
/// "name", and the drive-colon split then handed back `\src\repo`. That
/// spelling is ROOTED on Windows, and `Path::join` with a rooted path discards
/// the destination entirely -- so cloning into an existing directory resolved
/// to the source repository itself and refused with "Already cloned at
/// C:\src\repo". Both separators are split on for that reason.
pub fn repo_name_from_url(url: &str) -> String {
    let trimmed = url
        .trim()
        .trim_end_matches(['/', '\\'])
        .trim_end_matches(".git");
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .and_then(|s| s.rsplit(':').next())
        .filter(|s| !s.is_empty())
        .unwrap_or("repo")
        .to_string()
}

#[cfg(test)]
mod tests {
    use crate::procguard::LockedSpawn;
    /// S3-08. One status argv, shared by the file list and the worktree scans,
    /// and `status.showUntrackedFiles=all` pinned beside the other settings a
    /// developer's own config must not change.
    #[test]
    fn every_status_read_shares_one_argv_and_the_untracked_setting_is_pinned() {
        assert_eq!(
            crate::engine::git_reader::GitReader::STATUS_ARGV,
            super::STATUS_ARGV
        );
        assert!(super::STATUS_ARGV.contains(&"-u") && super::STATUS_ARGV.contains(&"-z"));
        assert!(super::PINNED_GIT_CONFIG.contains(&"status.showUntrackedFiles=all"));
        assert!(
            include_str!("git_reader.rs").contains("= crate::engine::git_cli::STATUS_ARGV;"),
            "GitReader::STATUS_ARGV must alias the shared constant, not restate it"
        );
        assert!(
            include_str!("worktree.rs").contains("crate::engine::git_cli::STATUS_ARGV.to_vec()"),
            "the worktree scans must read status through the shared constant"
        );
    }

    #[test]
    fn background_priority_restores_after_nested_work_errors_and_panic() {
        use super::{current_admission, Admission};
        assert_eq!(current_admission(), Admission::Reactive);
        let result = std::panic::catch_unwind(|| {
            super::with_background_processes(|| {
                assert_eq!(current_admission(), Admission::Background);
                let error: Result<(), &str> = super::with_background_processes(|| Err("expected"));
                assert_eq!(error, Err("expected"));
                assert_eq!(current_admission(), Admission::Background);
                panic!("expected priority unwind");
            })
        });
        assert!(result.is_err());
        assert_eq!(current_admission(), Admission::Reactive);
        super::with_background_processes(|| {
            std::thread::spawn(|| assert_eq!(current_admission(), Admission::Reactive))
                .join()
                .unwrap();
        });
        assert_eq!(current_admission(), Admission::Reactive);
    }

    /// The command guard promotes a mutation for the rest of its command and
    /// no further: the runner's scope ends the promotion, panics included, so
    /// a pooled thread never carries a user-action class into the next task.
    #[test]
    fn a_guarded_mutation_is_interactive_until_its_command_scope_ends() {
        use super::{current_admission, mark_interactive, with_admission, Admission};
        with_admission(Admission::Reactive, || {
            assert_eq!(current_admission(), Admission::Reactive);
            mark_interactive();
            assert_eq!(current_admission(), Admission::Interactive);
        });
        assert_eq!(current_admission(), Admission::Reactive);
        let unwound = std::panic::catch_unwind(|| {
            with_admission(Admission::Reactive, || {
                mark_interactive();
                panic!("mutation failed");
            })
        });
        assert!(unwound.is_err());
        assert_eq!(current_admission(), Admission::Reactive);
    }

    /// The launch failure: refresh traffic across ~20 repositories spent the
    /// rate budget, and the user's own actions were refused with it. A user
    /// action is limited by concurrency alone, and spends no token a refresh
    /// would need.
    #[test]
    fn a_user_action_is_admitted_after_refreshes_spend_every_token() {
        use super::{with_admission, Admission};
        let gate = super::SpawnGate::with_rate(8, 2, 1);
        let quick = || Instant::now() + std::time::Duration::from_millis(30);
        let burst: Vec<_> = (0..2)
            .map(|_| gate.acquire(quick()).expect("burst"))
            .collect();
        drop(burst);
        assert!(
            gate.acquire(quick()).is_none(),
            "the refresh budget is spent"
        );
        let actions: Vec<_> = (0..4)
            .map(|_| {
                with_admission(Admission::Interactive, || gate.acquire(quick()))
                    .expect("a user action must not wait for a rate token")
            })
            .collect();
        assert_eq!(gate.state.lock().unwrap().tokens, 0);
        drop(actions);
        let counters = gate.counters();
        assert_eq!(counters[Admission::Interactive.index()].admitted, 4);
        assert_eq!(counters[Admission::Interactive.index()].refused, 0);
        assert_eq!(counters[Admission::Reactive.index()].admitted, 2);
        assert_eq!(counters[Admission::Reactive.index()].refused, 1);

        // Still concurrency-limited: a user action is not a way past the
        // descriptor budget the gate exists for.
        let full: Vec<_> = (0..8)
            .map(|_| {
                with_admission(Admission::Interactive, || gate.acquire(quick())).expect("slot")
            })
            .collect();
        assert!(with_admission(Admission::Interactive, || gate.acquire(quick())).is_none());
        drop(full);
    }

    /// Background work goes first: once the budget is at the reserve kept for
    /// refreshes, it is refused at once rather than queued, while a refresh
    /// is still admitted from the reserve.
    #[test]
    fn background_is_shed_at_the_reserve_while_a_refresh_still_runs() {
        use super::{with_admission, with_background_processes, Admission, Refusal};
        let gate = super::SpawnGate::with_rate(16, 8, 1);
        assert_eq!(gate.background_reserve(), 2);
        let far = || Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..6 {
            drop(with_background_processes(|| gate.acquire(far())).expect("above reserve"));
        }
        let began = Instant::now();
        let shed = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
        assert_eq!(shed, Some(Refusal::Shed));
        assert!(
            began.elapsed()
                < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(500)),
            "shedding must not wait out the budget: {:?}",
            began.elapsed()
        );
        assert!(
            with_admission(Admission::Reactive, || gate.acquire(far())).is_some(),
            "the reserve is for refreshes"
        );
        assert_eq!(gate.counters()[Admission::Background.index()].shed, 1);
    }

    /// An agent's MCP read is background work that waits: at the reserve it is
    /// paced by the refill, not shed at 0.000s for a condition that clears in
    /// a fraction of a second. Shed work (the default, and the scheduled
    /// daemon) is the control, in the same gate and state.
    #[test]
    fn awaiting_background_work_is_paced_by_the_refill_where_shed_work_is_refused() {
        use super::{with_background_budget_wait, with_background_processes, Admission, Refusal};
        let gate = super::SpawnGate::with_rate(16, 8, 8);
        assert_eq!(gate.background_reserve(), 2);
        let far = || Instant::now() + std::time::Duration::from_secs(10);
        for _ in 0..6 {
            drop(with_background_processes(|| gate.acquire(far())).expect("above reserve"));
        }
        // Control: at the reserve, shed work is refused at once.
        let shed = with_background_processes(|| gate.acquire_until(far(), &|| false).err());
        assert_eq!(shed, Some(Refusal::Shed));
        // The same spawn, waiting: the next token (125 ms at 8/s) is admitted.
        let began = Instant::now();
        let permit = with_background_budget_wait(true, || {
            with_background_processes(|| gate.acquire_until(far(), &|| false))
        });
        assert!(
            permit.is_ok(),
            "a waiting background read was refused: {:?}",
            permit.err()
        );
        assert!(
            began.elapsed()
                < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(1_900)),
            "paced by the refill, not by the whole wait budget: {:?}",
            began.elapsed()
        );
        let counters = gate.counters()[Admission::Background.index()];
        assert_eq!(counters.shed, 1, "only the control was shed");
    }

    /// Waiting is bounded and never spends the reserve: with the budget empty
    /// and a refill too slow to clear the reserve inside the wait budget, the
    /// waiting read is refused after the same 2 s a reactive read gets, and a
    /// reactive read still has its token.
    #[test]
    fn awaiting_background_work_waits_a_bounded_time_and_leaves_the_reserve() {
        use super::{with_admission, with_background_budget_wait, with_background_processes};
        use super::{Admission, Refusal};
        let gate = super::SpawnGate::with_rate(16, 64, 1);
        assert_eq!(gate.background_reserve(), 16);
        let far = || Instant::now() + std::time::Duration::from_secs(30);
        for _ in 0..64 {
            drop(with_admission(Admission::Reactive, || gate.acquire(far())).expect("burst"));
        }
        let began = Instant::now();
        let refusal = with_background_budget_wait(true, || {
            with_background_processes(|| gate.acquire_until(far(), &|| false).err())
        });
        assert!(
            matches!(refusal, Some(Refusal::Refused { .. })),
            "expected a bounded wait that ends refused, got {refusal:?}"
        );
        assert!(
            began.elapsed() >= std::time::Duration::from_millis(1_900),
            "gave up before the wait budget: {:?}",
            began.elapsed()
        );
        assert!(
            with_admission(Admission::Reactive, || gate.acquire(far())).is_some(),
            "the waiting read did not spend what the refill gave back: reactive still admitted"
        );
    }

    /// Under memory pressure (the load governor's verdict) the gate sheds its
    /// own optional classes with the deferral marker the frontend retries on:
    /// background at `warn`, background and reactive at `critical` or a
    /// thrashing compressor. A user action is never shed, a read credited to
    /// one is not shed either, and everything resumes once the pressure
    /// clears. No spawn budget is spent on a shed spawn.
    #[test]
    fn memory_pressure_sheds_background_then_reactive_never_a_user_action_and_resumes() {
        use super::{with_admission, with_background_processes, Admission, Refusal};
        use crate::sysload::{GovernorInput, Level};
        let gate = super::SpawnGate::new(8);
        let far = || Instant::now() + std::time::Duration::from_secs(10);
        let background =
            || with_background_processes(|| gate.acquire_until(far(), &|| false).err());
        let reactive = || {
            with_admission(Admission::Reactive, || {
                gate.acquire_until(far(), &|| false).err()
            })
        };
        let interactive = || {
            with_admission(Admission::Interactive, || {
                gate.acquire_until(far(), &|| false).err()
            })
        };

        let calm = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Normal));
        assert_eq!(
            (background(), reactive(), interactive()),
            (None, None, None)
        );
        drop(calm);

        let warn = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Warn));
        assert_eq!(background(), Some(Refusal::Pressure { reactive: false }));
        assert_eq!((reactive(), interactive()), (None, None));
        drop(warn);

        let critical =
            crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
        assert_eq!(background(), Some(Refusal::Pressure { reactive: false }));
        assert_eq!(reactive(), Some(Refusal::Pressure { reactive: true }));
        assert_eq!(interactive(), None, "a click is never shed");
        let message = refusal_message("git status", Refusal::Pressure { reactive: true });
        assert!(
            is_deferred_under_load(message.message()),
            "{message}",
            message = message.message()
        );
        assert!(
            message.contains("memory pressure"),
            "{message}",
            message = message.message()
        );
        let counters = gate.counters();
        assert_eq!(counters[Admission::Background.index()].shed, 2);
        assert_eq!(counters[Admission::Reactive.index()].shed, 1);
        drop(critical);

        // A thrashing compressor sheds like critical while the level lags.
        let thrash = crate::sysload::inject_governor(GovernorInput {
            compressor_thrash: true,
            ..GovernorInput::test_pressure(Level::Normal)
        });
        assert_eq!(reactive(), Some(Refusal::Pressure { reactive: true }));
        drop(thrash);

        // Cleared: everything resumes.
        let _calm = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Normal));
        assert_eq!(
            (background(), reactive(), interactive()),
            (None, None, None)
        );
    }

    /// A waiting user action takes the next free slot even with refreshes
    /// queued ahead of it.
    #[test]
    fn a_waiting_user_action_takes_the_next_slot_before_a_queued_refresh() {
        use super::{with_admission, Admission};
        use std::sync::Arc;
        let gate = Arc::new(super::SpawnGate::new(1));
        let held = gate
            .acquire(Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        let (order_tx, order_rx) = std::sync::mpsc::channel();
        let refresh = {
            let gate = Arc::clone(&gate);
            let order_tx = order_tx.clone();
            std::thread::spawn(move || {
                let permit = gate.acquire(Instant::now() + std::time::Duration::from_secs(5));
                order_tx.send("refresh").unwrap();
                drop(permit);
            })
        };
        std::thread::sleep(std::time::Duration::from_millis(100));
        let action = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                with_admission(Admission::Interactive, || {
                    let permit = gate.acquire(Instant::now() + std::time::Duration::from_secs(5));
                    order_tx.send("action").unwrap();
                    std::thread::sleep(std::time::Duration::from_millis(50));
                    drop(permit);
                })
            })
        };
        let deadline = Instant::now() + std::time::Duration::from_secs(3);
        while gate.state.lock().unwrap().waiting_interactive == 0 {
            assert!(Instant::now() < deadline, "the action never queued");
            std::thread::yield_now();
        }
        drop(held);
        let first = order_rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .unwrap();
        assert_eq!(first, "action");
        refresh.join().unwrap();
        action.join().unwrap();
    }

    /// A deferral is recognized only in the shape the gate writes it, at the
    /// start (after the ledger's optional `[code] `), for every label the
    /// callers pass and every refusal wording. Text that quotes the phrase,
    /// in a path or a verdict, is not one.
    #[test]
    fn a_deferral_is_recognized_only_where_the_gate_writes_it() {
        use super::{is_deferred_under_load, refusal_message, Refusal};
        let waited = std::time::Duration::from_millis(1_500);
        for label in [
            "git worktree",
            "git status",
            "git check-ignore",
            "devmap",
            "kingfisher",
            "gh",
            "npm",
            "ps",
        ] {
            for refusal in [
                Refusal::Shed,
                Refusal::Refused { waited },
                Refusal::Pressure { reactive: true },
            ] {
                let message = refusal_message(label, refusal);
                assert!(
                    is_deferred_under_load(message.message()),
                    "{message}",
                    message = message.message()
                );
                assert!(
                    is_deferred_under_load(&format!(
                        "[deferred_worktree] {message}",
                        message = message.message()
                    )),
                    "{message}",
                    message = message.message()
                );
            }
        }
        let path = "/tmp/x deferred under load after 1.000s: y";
        for hostile in [
            format!("Not a Git repository: {path}"),
            format!("Worktree '{path}' does not belong to repository '/r'"),
            format!("fatal: cannot chdir to '{path}': No such file"),
            format!("[invalid_worktree] Not a Git repository: {path}"),
            format!("git status failed: {path}"),
            " deferred under load after 1s".to_string(),
            "git status deferred under load after soon".to_string(),
            "git status deferred under load after 1s'".to_string(),
        ] {
            assert!(!is_deferred_under_load(&hostile), "{hostile}");
        }
    }

    /// Each refusal carries exactly one marker, and the classifiers that the
    /// secrets runner and `run_captured` rely on agree with the formatter.
    #[test]
    fn every_refusal_formats_to_the_one_cause_its_classifiers_name() {
        use super::{
            is_deferred_under_load, is_slot_wait_timeout, is_timeout_error, refusal_message,
            Refusal,
        };
        let waited = std::time::Duration::from_millis(2_013);
        let deferred = refusal_message("git status", Refusal::Refused { waited });
        assert!(
            deferred.contains("2.013s"),
            "{deferred}",
            deferred = deferred.message()
        );
        let shed = refusal_message("git status", Refusal::Shed);
        for message in [&deferred, &shed] {
            assert!(
                is_deferred_under_load(message.message()),
                "{message}",
                message = message.message()
            );
            assert!(
                !is_slot_wait_timeout(message.message()),
                "{message}",
                message = message.message()
            );
            assert!(
                !is_timeout_error("git status", message.message()),
                "{message}",
                message = message.message()
            );
        }
        let timed_out = refusal_message(
            "git status",
            Refusal::TimedOut {
                deadline: std::time::Duration::from_secs(90),
            },
        );
        assert!(
            is_slot_wait_timeout(timed_out.message()),
            "{timed_out}",
            timed_out = timed_out.message()
        );
        assert!(
            is_timeout_error("git status", timed_out.message()),
            "{timed_out}",
            timed_out = timed_out.message()
        );
        assert!(
            !is_deferred_under_load(timed_out.message()),
            "{timed_out}",
            timed_out = timed_out.message()
        );
        let cancelled = refusal_message("git status", Refusal::Cancelled);
        assert!(
            !is_deferred_under_load(cancelled.message())
                && !is_slot_wait_timeout(cancelled.message())
        );
        let report = super::format_gate_report(
            &[super::AdmissionCounters::default(); 3],
            "budget=per-process",
        );
        for class in super::Admission::ALL {
            assert!(report.contains(class.name()), "{report}");
        }
        assert!(report.ends_with("| budget=per-process"), "{report}");
    }

    #[test]
    fn spawn_report_labels_never_carry_a_path_or_ref() {
        for (label, expected) in [
            ("git status", "git status"),
            ("git for-each-ref", "git for-each-ref"),
            ("gh", "gh"),
            ("devmap status --json", "devmap status"),
            ("git /Users/someone/private-repo", "git …"),
            ("git refs/heads/secret-branch", "git …"),
            ("git ../escape", "git …"),
            ("git .hidden", "git …"),
            ("git name with spaces", "git name"),
            ("", "…"),
            ("   ", "…"),
            (&"x".repeat(33), "…"),
            ("git sübcommand", "git …"),
        ] {
            assert_eq!(super::report_label(label), expected, "{label:?}");
        }
    }

    #[test]
    fn spawn_report_attribution_is_bounded_ordered_and_sums_the_rest() {
        let mut counts = super::LabelCounts::default();
        assert_eq!(counts.report(), "top: none");
        for _ in 0..5 {
            counts.note("git status");
        }
        for _ in 0..3 {
            counts.note("git for-each-ref");
        }
        counts.note("gh");
        counts.note("git /tmp/a");
        counts.note("git /tmp/b");
        // Sanitized labels merge rather than counting each path separately.
        assert_eq!(
            counts.report(),
            "top: git status=5 git for-each-ref=3 git …=2 gh=1"
        );

        // A stream of novel labels cannot grow the map past its bound.
        for i in 0..10_000 {
            counts.note(&format!("tool{i}"));
        }
        assert!(counts.counts.len() <= super::LabelCounts::MAX_LABELS + 1);
        let total: u64 = counts.counts.values().sum();
        assert_eq!(total, 5 + 3 + 1 + 2 + 10_000, "no admission is dropped");
        let report = counts.report();
        assert!(report.starts_with("top: other="), "{report}");
        assert!(report.contains(" rest="), "{report}");
        // One `label=count` per reported label, plus `rest=`; labels may
        // themselves contain a space ("git status"), so count the `=`.
        assert_eq!(
            report.matches('=').count(),
            super::LabelCounts::REPORTED + 1,
            "{report}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn admitted_spawns_are_attributed_to_their_label() {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(4)));
        for label in ["git status", "git status", "git log"] {
            let mut cmd = Command::new("/usr/bin/true");
            run_with_gate(
                &mut cmd,
                label,
                Duration::from_secs(10),
                None,
                1024,
                &mut (),
                gate,
            )
            .expect("spawn");
        }
        let report = gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_label
            .report();
        assert_eq!(report, "top: git status=2 git log=1");
    }

    #[test]
    fn spawn_rate_cap_stops_a_burst_from_becoming_a_sustained_storm() {
        let gate = super::SpawnGate::with_rate(8, 4, 4);
        let mut held = Vec::new();
        for _ in 0..4 {
            held.push(
                gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(200))
                    .expect("burst slot"),
            );
        }
        assert!(
            gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(80))
                .is_none(),
            "the fifth spawn inside the burst must not start"
        );
        drop(held);
        assert!(
            gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(80))
                .is_none(),
            "a free concurrency slot must not bypass the per-second cap"
        );
        let (burst, rate) = super::production_spawn_rate();
        assert!(
            rate < 40,
            "sustained cap {rate}/s is still inside the measured 40-80/s storm"
        );
        assert!(burst >= rate);
        assert_eq!(rate, 8);
        assert_eq!(burst, 192);
    }

    #[test]
    fn production_spawn_gate_spends_its_burst_and_then_stops() {
        let gate = super::configured_spawn_gate(false);
        let mut admitted = 0u32;
        let drain_until = std::time::Instant::now() + std::time::Duration::from_millis(400);
        while std::time::Instant::now() < drain_until {
            match gate.acquire(std::time::Instant::now() + std::time::Duration::from_millis(20)) {
                Some(permit) => {
                    admitted += 1;
                    drop(permit);
                }
                None => break,
            }
        }
        assert!(
            (192..192 + 16).contains(&admitted),
            "production burst admitted {admitted}, want 192 plus at most one refill window"
        );
    }

    #[test]
    fn excess_waiters_fail_at_the_queue_budget_instead_of_the_git_deadline() {
        use std::sync::Arc;
        let gate = Arc::new(super::SpawnGate::with_rate(32, 4, 1));
        let started = std::time::Instant::now();
        let mut threads = Vec::new();
        for _ in 0..12 {
            let gate = Arc::clone(&gate);
            threads.push(std::thread::spawn(move || {
                let began = std::time::Instant::now();
                let admitted = gate
                    .acquire(std::time::Instant::now() + std::time::Duration::from_secs(30))
                    .is_some();
                (admitted, began.elapsed())
            }));
        }
        let mut admitted = 0u32;
        let mut slowest = std::time::Duration::ZERO;
        for thread in threads {
            let (got, waited) = thread.join().unwrap();
            if got {
                admitted += 1;
            }
            slowest = slowest.max(waited);
        }
        assert!(
            started.elapsed()
                < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(4)),
            "join took {:?}; a refused spawn must not sit until the 90s git deadline",
            started.elapsed()
        );
        assert!(
            slowest < std::time::Duration::from_secs(3),
            "slowest acquire waited {slowest:?}"
        );
        assert!(
            (4..=8).contains(&admitted),
            "admitted {admitted}; burst 4 plus about two refill tokens, not all 12"
        );
    }

    #[test]
    fn spawn_rate_cap_bounds_a_concurrent_storm() {
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::sync::Arc;
        let gate = Arc::new(super::SpawnGate::with_rate(64, 8, 4));
        let admitted = Arc::new(AtomicU32::new(0));
        let started = std::time::Instant::now();
        let mut threads = Vec::new();
        for _ in 0..8 {
            let gate = Arc::clone(&gate);
            let admitted = Arc::clone(&admitted);
            threads.push(std::thread::spawn(move || {
                while started.elapsed()
                    < crate::test_support::coverage_relaxed(std::time::Duration::from_millis(500))
                {
                    if let Some(permit) = gate
                        .acquire(std::time::Instant::now() + std::time::Duration::from_millis(15))
                    {
                        admitted.fetch_add(1, Ordering::Relaxed);
                        drop(permit);
                    }
                }
            }));
        }
        for thread in threads {
            thread.join().unwrap();
        }
        // The cap is a rate, so it is measured against the window that actually
        // ran: coverage_relaxed stretches the 500 ms window tenfold under
        // instrumentation, and a bound fixed for 500 ms then fails on refills
        // the gate was right to grant. Windows up to a second keep the bound the
        // fixed version had (a second of refill plus 4 of slack); an uncapped
        // gate admits hundreds in the same window.
        let window = started.elapsed().as_secs_f64();
        let n = admitted.load(Ordering::Relaxed);
        let cap = 8 + (4.0 * window.max(1.0)).ceil() as u32 + 4;
        assert!(n >= 8, "burst was not issued: {n}");
        assert!(
            n <= cap,
            "concurrent storm admitted {n} in {window:.2}s; cap is burst 8 plus 4/s = {cap}"
        );
    }

    #[test]
    fn background_waiter_gets_capacity_and_timeout_releases_its_reservation() {
        let gate = std::sync::Arc::new(super::SpawnGate::new(4));
        let mut foreground: Vec<_> = (0..4)
            .map(|_| {
                gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect();
        let worker_gate = gate.clone();
        let (acquired_tx, acquired_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker = std::thread::spawn(move || {
            super::with_background_processes(|| {
                let _permit = worker_gate
                    .acquire(Instant::now() + std::time::Duration::from_secs(5))
                    .expect("background must eventually run");
                acquired_tx.send(()).unwrap();
                release_rx
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
            })
        });
        let state = gate.state.lock().unwrap();
        let (state, waited) = gate
            .released
            .wait_timeout_while(state, std::time::Duration::from_secs(3), |state| {
                state.waiting_background == 0
            })
            .unwrap();
        assert!(!waited.timed_out());
        drop(state);
        foreground.pop();
        acquired_rx
            .recv_timeout(std::time::Duration::from_secs(3))
            .unwrap();
        assert!(gate
            .acquire(Instant::now() + std::time::Duration::from_millis(20))
            .is_none());
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        let last = gate
            .acquire(Instant::now() + std::time::Duration::from_secs(1))
            .unwrap();
        assert!(super::with_background_processes(
            || gate.acquire(Instant::now() + std::time::Duration::from_millis(20))
        )
        .is_none());
        assert_eq!(gate.state.lock().unwrap().waiting_background, 0);
        drop(last);
        assert!(gate
            .acquire(Instant::now() + std::time::Duration::from_secs(1))
            .is_some());
    }

    #[test]
    fn mixed_priority_contention_preserves_counts_and_makes_progress() {
        let gate = std::sync::Arc::new(super::SpawnGate::new(8));
        let workers: Vec<_> = (0..24)
            .map(|worker| {
                let gate = gate.clone();
                std::thread::spawn(move || {
                    let run = || {
                        for _ in 0..50 {
                            let _permit = gate
                                .acquire(Instant::now() + std::time::Duration::from_secs(10))
                                .unwrap();
                            {
                                let state = gate.state.lock().unwrap();
                                assert!(state.in_flight <= 8);
                                assert!(state.background <= 2);
                            }
                            std::thread::yield_now();
                        }
                    };
                    if worker % 2 == 0 {
                        super::with_background_processes(run);
                    } else {
                        run();
                    }
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap();
        }
        let state = gate.state.lock().unwrap();
        assert_eq!(
            (state.in_flight, state.background, state.waiting_background),
            (0, 0, 0)
        );
        assert!(state.peak <= 8);
    }

    #[test]
    fn background_process_admission_reserves_foreground_capacity() {
        let gate = super::SpawnGate::new(4);
        let first = super::with_background_processes(|| {
            gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
        })
        .expect("first background slot");
        let excess = super::with_background_processes(|| {
            gate.acquire(Instant::now() + std::time::Duration::from_millis(20))
        });
        assert!(
            excess.is_none(),
            "background work must not consume the foreground reserve"
        );
        let foreground: Vec<_> = (0..3)
            .map(|_| {
                gate.acquire(Instant::now() + std::time::Duration::from_secs(1))
                    .unwrap()
            })
            .collect();
        assert_eq!(gate.peak(), 4);
        drop(foreground);
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn audit_failure_diagnostics_bound_stderr_as_well_as_stdout() {
        let dir = crate::test_support::git_repo();
        let error = git_run_capped(
            Some(dir.path()),
            &[
                "-c",
                "alias.noisy=!head -c 10000 /dev/zero >&2; exit 1",
                "noisy",
            ],
            Duration::from_secs(5),
            None,
            1024,
        )
        .unwrap_err();
        assert!(
            error.message().len() < MAX_FAILURE_MESSAGE_BYTES + 100,
            "unbounded failure message: {} bytes",
            error.message().len()
        );
        assert!(error.contains("output truncated"));
    }

    /// End to end: a failing command whose cause is its last line keeps that
    /// line in the error, however much it printed before it.
    #[cfg(unix)]
    #[test]
    fn failure_diagnosis_keeps_the_last_line_of_long_output() {
        let dir = crate::test_support::git_repo();
        let error = git_run_capped(
            Some(dir.path()),
            &[
                "-c",
                "alias.noisy=!yes chatter | head -n 2000 >&2; echo 'error: failed to push some refs' >&2; exit 1",
                "noisy",
            ],
            Duration::from_secs(5),
            None,
            1024,
        )
        .unwrap_err();
        assert!(
            error.message().starts_with("chatter"),
            "{error}",
            error = error.message()
        );
        assert!(
            error.message().ends_with("error: failed to push some refs"),
            "failing tail dropped: …{}",
            &error.message()[error.message().len().saturating_sub(120)..]
        );
    }
    #[test]
    #[cfg(unix)]
    fn audit_capture_refuses_incomplete_stderr() {
        let result = capture_command(
            "sh",
            &["-c", "echo version >&2; sleep 3 >/dev/null &"],
            None,
            Duration::from_secs(5),
            &[],
        );
        assert!(result.is_err(), "capture erased stderr completeness");
    }
    #[cfg(unix)]
    const RETRYING_ALIAS: &str =
        "alias.retry=!echo attempt >> attempts; sleep 0.08; echo \"fatal: Unable to create 'x.lock': File exists.\" >&2; exit 1";

    /// Where the total budget runs out between two lock retries decides which
    /// seam reports it: the top of the loop returns an error, a child killed
    /// in flight returns its prefix tagged [`Incomplete::Deadline`]. Which one
    /// happens depends on the backoff (pid-derived) and on scheduling, so a
    /// capped caller must accept either, and a caller that needs the whole
    /// stream must get an error from both.
    #[cfg(unix)]
    #[test]
    fn audit_lock_retries_share_one_total_deadline() {
        let dir = crate::test_support::git_repo();
        let started = Instant::now();
        let capped = git_run_capped(
            Some(dir.path()),
            &["-c", RETRYING_ALIAS, "retry"],
            Duration::from_millis(150),
            None,
            1024,
        );
        let whole = git_timeout(
            Some(dir.path()),
            &["-c", RETRYING_ALIAS, "retry"],
            Duration::from_millis(150),
            None,
        );
        // Two sequential runs of one 150 ms budget each.
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_millis(1000)),
            "retries renewed the timeout: {:?}",
            started.elapsed()
        );
        match capped {
            Err(error) => assert!(
                error.contains("timed out"),
                "{error}",
                error = error.message()
            ),
            Ok((_, Some(Incomplete::Deadline { message, .. }))) => {
                assert!(message.contains("timed out"), "{message}")
            }
            other => panic!("a deadline must never look like success: {other:?}"),
        }
        let error = whole.expect_err("a caller that needs the whole stream cannot accept a prefix");
        assert!(
            error.contains("timed out"),
            "deadline expiry must be explicit: {error}",
            error = error.message()
        );
    }

    /// The in-flight half, deterministically: no lock error, so the budget can
    /// only run out inside the child.
    #[cfg(unix)]
    #[test]
    fn audit_a_deadline_in_flight_is_a_tagged_prefix_for_capped_callers() {
        let dir = crate::test_support::git_repo();
        let args = ["-c", "alias.slow=!sleep 5", "slow"];
        let capped = git_run_capped(
            Some(dir.path()),
            &args,
            Duration::from_millis(100),
            None,
            1024,
        )
        .expect("a killed child is a tagged prefix, not an error");
        assert!(
            matches!(capped.1, Some(Incomplete::Deadline { .. })),
            "{:?}",
            capped.1
        );
        let error = git_timeout(Some(dir.path()), &args, Duration::from_millis(100), None)
            .expect_err("whole-stream callers fail a deadline");
        assert!(
            error.contains("timed out"),
            "{error}",
            error = error.message()
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_zero_deadline_never_starts_a_command() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "touch \"$1\"", "test"]).arg(&marker);
        assert!(run_bounded(cmd, "sh", Duration::ZERO, None).is_err());
        assert!(
            !marker.exists(),
            "an expired request must not mutate anything"
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_admission_wait_consumes_the_command_deadline() {
        // Exercise the production runner with an isolated gate. Acquiring
        // the global gate one permit at a time can monopolize most slots
        // while waiting for the last, starving unrelated concurrent tests.
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
        let permits: Vec<_> = (0..gate.limit)
            .map(|_| {
                gate.acquire(Instant::now() + Duration::from_secs(30))
                    .unwrap()
            })
            .collect();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("started");
        let copy = marker.clone();
        let worker = thread::spawn(move || {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "touch \"$1\"", "test"]).arg(copy);
            run_with_gate(
                &mut cmd,
                "sh",
                Duration::from_millis(50),
                None,
                1024,
                &mut (),
                gate,
            )
        });
        thread::sleep(Duration::from_millis(300));
        drop(permits);
        let result = worker.join().unwrap();
        assert!(result.is_err(), "queued request ran after its deadline");
        assert!(!marker.exists());
    }

    #[cfg(unix)]
    #[test]
    fn audit_retained_stdin_does_not_block_after_child_exit() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "exec 3<&0; sleep 3 <&3 >/dev/null 2>&1 & exit 0"]);
        let started = Instant::now();
        let result = run_bounded(
            cmd,
            "sh",
            Duration::from_secs(1),
            Some(&vec![b'x'; 1024 * 1024]),
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "stdin join outlived the child: {:?}",
            started.elapsed()
        );
        assert!(
            result.is_err(),
            "undelivered stdin is not successful delivery"
        );
    }

    #[cfg(unix)]
    #[test]
    fn audit_stderr_cap_is_reported() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "head -c 5000000 /dev/zero >&2"]);
        let out = run_bounded(cmd, "sh", Duration::from_secs(10), None).unwrap();
        assert!(out.success && out.incomplete.is_none());
        assert!(String::from_utf8_lossy(&out.stderr).contains("stderr incomplete"));
    }

    #[cfg(unix)]
    #[test]
    fn audit_output_cap_cannot_disable_the_global_memory_bound() {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf ok"]);
        assert!(run_bounded_capped(cmd, "sh", Duration::from_secs(5), None, usize::MAX).is_err());
    }

    /// A descendant retaining the write ends must not erase bytes the parent
    /// already emitted. The result remains explicitly incomplete until EOF.
    #[cfg(unix)]
    #[test]
    fn inherited_output_pipes_preserve_the_captured_prefix() {
        let mut cmd = Command::new("sh");
        cmd.args([
            "-c",
            "printf payload; printf diagnostic >&2; sleep 6 & exit 0",
        ]);
        let out = run_bounded(cmd, "sh", Duration::from_secs(10), None).expect("run");
        assert!(out.success);
        assert!(matches!(out.incomplete, Some(Incomplete::Unread(_))));
        assert_eq!(out.stdout, b"payload");
        assert!(out.stderr.starts_with(b"diagnostic"));
    }

    #[test]
    fn interrupted_output_reads_resume_without_losing_bytes() {
        struct InterruptedOnce(bool);
        impl Read for InterruptedOnce {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                if !self.0 {
                    self.0 = true;
                    return Err(std::io::ErrorKind::Interrupted.into());
                }
                bytes[0] = b'x';
                Ok(1)
            }
        }
        // Stop after the first real byte so the synthetic source is finite.
        let result = drain_capped(Some(InterruptedOnce(false).take(1)), 8);
        assert_eq!(result.bytes, b"x");
        assert!(result.stop.is_none());
    }

    use super::*;

    #[test]
    fn poll_backoff_starts_far_below_the_old_fixed_quantum() {
        // The regression this replaces: every child was polled on a flat 15 ms
        // schedule, so a 2 ms `git rev-parse` took 15 ms of wall clock. The
        // first sleep has to be small enough that a fast command is observed
        // promptly rather than on the next tick.
        assert!(POLL_BACKOFF_START <= Duration::from_millis(1));
        assert!(
            POLL_BACKOFF_START > Duration::ZERO,
            "a zero sleep is a spin"
        );
    }

    #[test]
    fn poll_backoff_doubles_and_stops_at_the_ceiling() {
        let mut d = POLL_BACKOFF_START;
        let mut steps = 0;
        while d < POLL_BACKOFF_MAX {
            let next = next_poll_backoff(d);
            assert!(next > d, "backoff must make progress from {d:?}");
            d = next;
            steps += 1;
            assert!(steps < 64, "backoff never reached its ceiling");
        }
        assert_eq!(d, POLL_BACKOFF_MAX);
        // Saturating at the ceiling is what keeps a long clone off a hot loop.
        assert_eq!(next_poll_backoff(POLL_BACKOFF_MAX), POLL_BACKOFF_MAX);
    }

    #[test]
    fn reaching_the_ceiling_costs_a_bounded_number_of_polls() {
        // The whole ramp must be cheap: if getting to the ceiling took
        // thousands of wakeups, the fast case would be paid for by every long
        // running command.
        let mut d = POLL_BACKOFF_START;
        let mut polls = 0;
        let mut slept = Duration::ZERO;
        while d < POLL_BACKOFF_MAX {
            slept += d;
            d = next_poll_backoff(d);
            polls += 1;
        }
        assert!(polls <= 16, "ramp took {polls} polls");
        // A doubling ramp's total is about twice its last step, so the whole
        // warm-up costs on the order of ONE old tick — not orders of magnitude
        // more. That is the property that makes the fast case free rather than
        // borrowed from long-running commands.
        assert!(
            slept <= POLL_BACKOFF_MAX * 2,
            "the entire ramp ({slept:?}) should cost about one old tick"
        );
    }

    #[test]
    fn test_sandbox_rejects_parent_dir() {
        let repo = Path::new("/tmp/example-repo");
        assert!(sandbox_join(repo, "../secret").is_err());
        assert!(sandbox_join(repo, "/etc/passwd").is_err());
        assert!(sandbox_join(repo, "src/main.rs").is_ok());
    }

    #[test]
    fn transient_lock_errors_match_real_git_wording() {
        assert!(is_transient_git_lock_error(
            "fatal: Unable to create '/tmp/repo/.git/index.lock': File exists.\n\nAnother git process seems to be running in this repository"
        ));
        assert!(is_transient_git_lock_error(
            "fatal: cannot lock ref 'refs/heads/main': Unable to create '/tmp/repo/.git/refs/heads/main.lock': File exists."
        ));
        assert!(!is_transient_git_lock_error(
            "nothing to commit, working tree clean"
        ));
        assert!(!is_transient_git_lock_error(
            "pathspec 'ghost.txt' did not match any files"
        ));
        assert!(lock_retry_backoff_ms(1) >= 50);
        assert!(lock_retry_backoff_ms(2) >= 150);
        assert!(lock_retry_backoff_ms(3) >= 300);
    }

    /// Every message shape the classifier is handed: raw from `git_run`, and
    /// wrapped by `git_writer` ("Index update failed for the N selected
    /// paths: ...") before it asks whether to halve a batch.
    #[test]
    fn lock_retry_classifier_reads_only_gits_own_first_line() {
        let lock = "Unable to create '/r/.git/index.lock': File exists.";
        let ref_lock = "cannot lock ref 'refs/heads/main': Unable to create '/r/.git/refs/heads/main.lock': File exists.";
        // Raw, git's own: retried.
        assert!(is_transient_git_lock_error(&format!(
            "fatal: {lock}\n\nAnother git process seems to be running in this repository"
        )));
        assert!(is_transient_git_lock_error(&format!("error: {ref_lock}")));
        // Wrapped by git_writer: still git's lock on the first line.
        assert!(is_transient_git_lock_error(&format!(
            "Index update failed for the 2 selected paths: fatal: {lock}\n\nAnother git process seems to be running. Refresh to review the current index."
        )));
        // A server's output relayed by push or fetch.
        assert!(!is_transient_git_lock_error(
            "remote: error: cannot lock ref 'refs/heads/main': is at A but expected B\nTo origin\n ! [remote rejected]"
        ));
        assert!(!is_transient_git_lock_error(&format!(
            "remote: fatal: {lock}\nTo origin"
        )));
        // Hook output ahead of git's own words: the lock is not on line one.
        assert!(!is_transient_git_lock_error(&format!(
            "running lint...\nfatal: {lock}\nhook failed"
        )));
        // Permanent ref-lock forms.
        assert!(!is_transient_git_lock_error(
            "error: cannot lock ref 'refs/heads/a/b': 'refs/heads/a' exists; cannot create 'refs/heads/a/b'\nerror: unable to update local ref"
        ));
        assert!(!is_transient_git_lock_error(
            "error: cannot lock ref 'refs/heads/main': is at A but expected B"
        ));
    }

    #[test]
    fn lock_retry_never_replays_hook_or_network_commands() {
        for sub in [
            "commit",
            "merge",
            "pull",
            "rebase",
            "push",
            "fetch",
            "clone",
            "ls-remote",
        ] {
            assert!(!lock_retry_allowed(sub), "{sub}");
        }
        for sub in ["add", "update-ref", "reset", "branch", "status"] {
            assert!(lock_retry_allowed(sub), "{sub}");
        }
    }

    /// The real directory/file conflict is permanent: git's own wording does
    /// not classify as a lock and the command is not run again.
    #[test]
    fn a_real_ref_directory_file_conflict_is_not_a_lock() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        crate::test_support::commit_at(dir.path(), "c", 1_700_000_000, "a@example.com");
        git_text(dir.path(), &["update-ref", "refs/heads/a", "HEAD"]).unwrap();
        let started = Instant::now();
        let error = git_text(dir.path(), &["update-ref", "refs/heads/a/b", "HEAD"])
            .expect_err("a/b cannot coexist with a");
        assert!(
            error.contains("exists; cannot create"),
            "{error}",
            error = error.message()
        );
        assert!(
            !is_transient_git_lock_error(error.message()),
            "{error}",
            error = error.message()
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "a permanent failure is not retried with backoff"
        );
    }

    /// A pre-commit hook that fails with a lock-shaped message ran once: the
    /// command is not replayed. Counted by the hook itself, so a replay cannot
    /// hide behind a fast retry.
    #[cfg(unix)]
    #[test]
    fn a_failing_commit_hook_runs_exactly_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        git_text(dir.path(), &["config", "user.name", "T"]).unwrap();
        git_text(dir.path(), &["config", "user.email", "t@example.com"]).unwrap();
        std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
        git_text(dir.path(), &["add", "f.txt"]).unwrap();
        let counter = dir.path().join("hook-count");
        // A hook whose nested git, against THIS repository, prints a bare lock
        // line: git's advice block is missing, so it is not git's own failure.
        let hook = dir.path().join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho run >> '{}'\necho \"fatal: Unable to create '{}/.git/index.lock': File exists.\" >&2\nexit 1\n",
                counter.display(),
                dir.path().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        let error = git_text(dir.path(), &["commit", "-m", "x"]).expect_err("hook rejects");
        assert!(
            is_transient_git_lock_error(error.message()),
            "{error}",
            error = error.message()
        );
        let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(runs, 1, "the hook ran {runs} times");
    }

    /// The lint-staged shape: the hook's nested git against THIS repository
    /// fails on the lock, prints git's advice block as well, and the hook then
    /// reports its own failure. Other output follows git's lines, so something
    /// ran: the commit and its hook are not replayed.
    #[cfg(unix)]
    #[test]
    fn a_hook_quoting_this_repos_lock_with_advice_and_other_output_runs_once() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        git_text(dir.path(), &["config", "user.name", "T"]).unwrap();
        git_text(dir.path(), &["config", "user.email", "t@example.com"]).unwrap();
        std::fs::write(dir.path().join("f.txt"), "x\n").unwrap();
        git_text(dir.path(), &["add", "f.txt"]).unwrap();
        let counter = dir.path().join("hook-count");
        let hook = dir.path().join(".git/hooks/pre-commit");
        std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
        std::fs::write(
            &hook,
            format!(
                "#!/bin/sh\necho run >> '{c}'\necho \"fatal: Unable to create '{d}/.git/index.lock': File exists.\" >&2\necho >&2\necho 'Another git process seems to be running in this repository, or the lock file may be stale' >&2\necho 'lint-staged failed' >&2\nexit 1\n",
                c = counter.display(),
                d = dir.path().display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();

        git_text(dir.path(), &["commit", "-m", "x"]).expect_err("hook rejects");
        let runs = std::fs::read_to_string(&counter).unwrap().lines().count();
        assert_eq!(runs, 1, "the hook ran {runs} times");
    }

    /// git's advice under a lock failure, as printed by real git 2.54 and 2.56
    /// (identical), plus the five-line block older git printed. Only the lock
    /// line and that block, nothing else, is git's own index.lock failure.
    #[test]
    fn only_gits_own_index_lock_failure_qualifies_for_a_hooked_retry() {
        let line = "fatal: Unable to create '/r/.git/index.lock': File exists.";
        let current = "Another git process seems to be running in this repository, or the lock file may be stale";
        let legacy = "Another git process seems to be running in this repository, e.g.\nan editor opened by 'git commit'. Please make sure all processes\nare terminated then try again. If it still fails, a git process\nmay have crashed in this repository earlier:\nremove the file manually to continue.";
        assert!(consists_only_of_gits_index_lock_failure(&format!(
            "{line}\n\n{current}"
        )));
        assert!(consists_only_of_gits_index_lock_failure(&format!(
            "{line}\n\n{current}\n"
        )));
        assert!(consists_only_of_gits_index_lock_failure(&format!(
            "{line}\n\n{legacy}"
        )));
        // The `error:` first line and trailing lines real merge, cherry-pick,
        // revert and rebase print.
        let error_line = "error: Unable to create '/r/.git/index.lock': File exists.";
        for followup in [
            "fatal: Unable to write index.",
            "fatal: cherry-pick failed",
            "fatal: revert failed",
            "error: could not detach HEAD",
        ] {
            assert!(
                consists_only_of_gits_index_lock_failure(&format!(
                    "{error_line}\n\n{current}\n{followup}"
                )),
                "{followup}"
            );
        }
        // A bare line, any other output, a ref lock, or a non-git first line.
        assert!(!consists_only_of_gits_index_lock_failure(line));
        assert!(!consists_only_of_gits_index_lock_failure(&format!(
            "{line}\n\n{current}\nlint failed"
        )));
        assert!(!consists_only_of_gits_index_lock_failure(&format!(
            "lint failed\n{line}\n\n{current}"
        )));
        assert!(!consists_only_of_gits_index_lock_failure(&format!(
            "{line}\n\n{current} and more"
        )));
        assert!(!consists_only_of_gits_index_lock_failure(&format!(
            "error: cannot lock ref 'HEAD': Unable to create '/r/.git/HEAD.lock': File exists.\n\n{current}"
        )));
        assert!(!consists_only_of_gits_index_lock_failure(&format!(
            "remote: {line}\n\n{current}"
        )));
    }

    /// The timeout and spawn shapes every converged classifier must tell apart,
    /// raw and wrapped the ways callers wrap them.
    #[test]
    fn run_error_classifies_each_runner_message_shape() {
        let slot = format!("devmap{TIMEOUT_MARKER}0.5{SLOT_WAIT_SUFFIX}");
        let ran_out = format!("devmap{TIMEOUT_MARKER}30s");
        let deferred = refusal_message(
            "devmap build",
            Refusal::Refused {
                waited: Duration::from_millis(500),
            },
        );
        let hostile = "Failed to spawn /Users/x/timeout-wrapper/devmap: no such file";
        let hostile_marker =
            format!("Failed to spawn /Users/x/devmap{TIMEOUT_MARKER}1s/devmap: permission denied");
        let not_found = spawn_error(
            "/Users/x/timeout/devmap",
            &std::io::Error::from(std::io::ErrorKind::NotFound),
        );
        assert_eq!(RunError::classify(&ran_out), RunError::Timeout);
        assert_eq!(
            RunError::classify(&format!("[some_code] {ran_out}")),
            RunError::Timeout,
            "a coded prefix does not hide a timeout"
        );
        assert_eq!(
            RunError::classify(&format!("[some_code] {slot}")),
            RunError::SlotWaitTimeout
        );
        assert_eq!(RunError::classify(&slot), RunError::SlotWaitTimeout);
        assert_eq!(RunError::classify(deferred.message()), RunError::Deferred);
        assert_eq!(
            RunError::classify(&format!(
                "[deferred_worktree] {deferred}",
                deferred = deferred.message()
            )),
            RunError::Deferred
        );
        assert_eq!(RunError::classify(hostile), RunError::SpawnFailed);
        assert_eq!(RunError::classify(&hostile_marker), RunError::SpawnFailed);
        assert_eq!(RunError::classify(&not_found), RunError::SpawnNotFound);
        assert_eq!(
            RunError::classify("devmap exited with status 2"),
            RunError::Other
        );
        assert!(is_run_timeout(&ran_out));
        for not_timeout in [slot.as_str(), deferred.message(), not_found.as_str()] {
            assert!(!is_run_timeout(not_timeout), "{not_timeout}");
        }
        assert!(!is_run_timeout(hostile));
        assert!(!is_run_timeout(&hostile_marker));
    }

    #[test]
    fn test_parse_ahead_behind() {
        assert_eq!(parse_ahead_behind("[ahead 3, behind 1]"), (3, 1));
        assert_eq!(parse_ahead_behind("[ahead 12]"), (12, 0));
        assert_eq!(parse_ahead_behind("[behind 4]"), (0, 4));
        assert_eq!(parse_ahead_behind("[gone]"), (0, 0));
        assert_eq!(parse_ahead_behind(""), (0, 0));
        assert_eq!(parse_ahead_behind("[ahead 0, behind 0]"), (0, 0));
        assert_eq!(parse_ahead_behind("[ahead 1234, behind 99]"), (1234, 99));
        // Digits must be consumed without an intermediate String.
        assert_eq!(parse_ahead_behind("ahead 7 behind 8 extra"), (7, 8));
    }

    #[test]
    fn test_upstream_is_gone() {
        assert!(upstream_is_gone("[gone]"));
        assert!(upstream_is_gone("[ahead 1, gone]"));
        assert!(!upstream_is_gone("[ahead 3, behind 1]"));
        assert!(!upstream_is_gone(""));
    }

    #[test]
    fn test_parse_left_right_count() {
        assert_eq!(parse_left_right_count("2\t5"), (2, 5));
        assert_eq!(parse_left_right_count("0\t0\n"), (0, 0));
        assert_eq!(parse_left_right_count("12 3"), (12, 3));
        assert_eq!(parse_left_right_count(""), (0, 0));
    }

    #[test]
    fn test_repo_name_from_url() {
        assert_eq!(
            repo_name_from_url("https://github.com/acme/gitpulse.git"),
            "gitpulse"
        );
        assert_eq!(
            repo_name_from_url("git@github.com:acme/gitpulse.git"),
            "gitpulse"
        );
    }

    /// A local Windows path is an ordinary clone source, and its last
    /// component is its name like anywhere else.
    #[test]
    fn repo_name_from_a_windows_path_is_its_last_component() {
        assert_eq!(
            repo_name_from_url(r"C:\Users\me\AppData\Local\Temp\.tmpAbC"),
            ".tmpAbC"
        );
        assert_eq!(repo_name_from_url(r"C:\src\gitpulse.git"), "gitpulse");
        assert_eq!(repo_name_from_url(r"\\server\share\gitpulse"), "gitpulse");
        assert_eq!(repo_name_from_url(r"C:\src\repo\"), "repo");
        assert_eq!(repo_name_from_url(r"C:\src\repo\\"), "repo");
    }

    /// The property the clone destination depends on: whatever comes back is
    /// ONE component, so joining it onto the chosen destination cannot land
    /// anywhere else. A rooted or separator-bearing answer silently replaced
    /// the destination, which is exactly how a clone came to resolve onto its
    /// own source.
    #[test]
    fn a_derived_repo_name_can_never_redirect_the_join() {
        let base = if cfg!(windows) {
            PathBuf::from(r"C:\clone-base")
        } else {
            PathBuf::from("/clone-base")
        };
        for url in [
            r"C:\Users\me\AppData\Local\Temp\.tmpAbC",
            r"C:\src\repo",
            r"\\server\share\repo",
            "C:",
            "https://github.com/acme/gitpulse.git",
            "git@github.com:acme/gitpulse.git",
            "/tmp/.tmpAbC",
            "/",
            "",
            "   ",
            "file:///tmp/src",
        ] {
            let name = repo_name_from_url(url);
            assert!(
                !name.contains('/') && !name.contains('\\'),
                "{url:?} produced {name:?}, which is more than one component"
            );
            assert!(
                !Path::new(&name).is_absolute() && !Path::new(&name).has_root(),
                "{url:?} produced the rooted name {name:?}"
            );
            assert!(
                base.join(&name).starts_with(&base),
                "{url:?} produced {name:?}, which joins outside the destination"
            );
        }
    }

    /// A spawn is named by its command, not by the first global option before
    /// it: the gate's report and every error used to say `git -c`.
    #[test]
    fn a_spawn_is_named_by_its_command_past_global_options() {
        assert_eq!(subcommand(&["diff", "--shortstat"]), "diff");
        assert_eq!(subcommand(&["-c", "core.quotepath=off", "diff"]), "diff");
        assert_eq!(subcommand(&["--attr-source=0123abcd", "diff"]), "diff");
        assert_eq!(subcommand(&["--no-pager", "-C", "/x", "log"]), "log");
        assert_eq!(subcommand(&["-c"]), "");
        assert_eq!(subcommand(&[]), "");

        let dir = tempfile::tempdir().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-q"]);
        crate::test_support::trust_repo(dir.path());
        // `--quiet` makes git print nothing, so the error is the one built
        // from the spawn's own name.
        let err = git_text(
            dir.path(),
            &[
                "-c",
                "core.quotepath=off",
                "rev-parse",
                "--verify",
                "--quiet",
                "no-such-ref",
            ],
        )
        .unwrap_err();
        assert!(err.contains("git rev-parse"), "{err}", err = err.message());
        assert!(!err.contains("git -c"), "{err}", err = err.message());
    }

    #[test]
    fn test_validate_repo_rejects_empty() {
        assert!(validate_repo("").is_err());
        assert!(validate_repo("relative/path").is_err());
        assert!(validate_repo("/tmp/foo\0bar").is_err());
        assert!(validate_repo("/tmp/foo\nbar").is_err());
        assert!(validate_repo("/definitely/missing-gitpulse-validate-repo").is_err());
    }

    #[test]
    fn run_captured_reports_a_finished_run_with_tails() {
        let outcome = run_captured(
            "git",
            &["--version"],
            None,
            DEFAULT_TIMEOUT,
            &[],
            MAX_OUTPUT_BYTES,
        )
        .unwrap();
        match outcome {
            RunOutcome::Finished(run) => {
                assert!(run.success);
                assert_eq!(run.status_code, 0);
                assert!(run.stdout_tail.contains("git version"));
                assert!(!run.truncated);
            }
            RunOutcome::TimedOut(_) => panic!("git --version must not time out"),
        }
    }

    /// Pins the timeout contract of [`run_captured`]: a deadline kill is an
    /// outcome (`TimedOut`), never laundered into a spawn-style error. If
    /// `run_bounded`'s message wording changes, this fails and the detector
    /// in `run_captured` has to be updated with it.
    #[cfg(unix)]
    #[test]
    fn run_captured_reports_timeout_as_an_outcome() {
        let outcome = run_captured(
            "sleep",
            &["5"],
            None,
            Duration::from_secs(1),
            &[],
            MAX_OUTPUT_BYTES,
        )
        .unwrap();
        assert!(
            matches!(outcome, RunOutcome::TimedOut(d) if d.as_secs() == 1),
            "expected TimedOut(1s), got {outcome:?}"
        );
    }

    #[test]
    fn byte_tail_keeps_the_end_and_stays_char_safe() {
        assert_eq!(byte_tail(b"hello world", 5), "world");
        // A multibyte sequence split at the cap must not leak replacement
        // characters at the head of the tail.
        let prefix = "é".repeat(5000); // 2 bytes each
        assert!(!byte_tail(prefix.as_bytes(), 9999).starts_with('\u{FFFD}'));
    }

    /// "Not installed" has to be earned: only the OS failing to find the
    /// program says it. A spawn refused under load said it too, which is how
    /// the Health panel told a user with `gh` on PATH to install it.
    #[test]
    fn a_tool_probe_says_not_found_only_when_the_os_found_nothing() {
        // A real spawn of a name that resolves nowhere: the one "no".
        let missing = capture_command_with_env(
            "gitpulse-no-such-tool-7f3a",
            &["--version"],
            None,
            Duration::from_secs(5),
            &[],
            Some(std::ffi::OsStr::new("")),
            None,
            PathSource::Inherited,
        );
        match classify_tool_probe("gitpulse-no-such-tool-7f3a", missing) {
            ToolProbe::NotFound(error) => {
                assert!(
                    error.starts_with("Failed to spawn gitpulse-no-such-tool-7f3a"),
                    "{error}"
                );
                assert!(is_spawn_not_found(&error), "{error}");
            }
            other => panic!("a program that is nowhere is not found, got {other:?}"),
        }

        // Shed under load, refused by the rate limit, no slot in time,
        // cancelled: none of these started anything, so none is an absence.
        for refusal in [
            Refusal::Shed,
            Refusal::Refused {
                waited: Duration::from_millis(1500),
            },
            Refusal::TimedOut {
                deadline: Duration::from_secs(2),
            },
            Refusal::Cancelled,
        ] {
            let error = refusal_message("gh", refusal);
            assert!(
                matches!(
                    classify_tool_probe("gh", Err(error.clone())),
                    ToolProbe::NotRun(_)
                ),
                "{error}",
                error = error.message()
            );
        }

        // A spawn the OS refused for a reason other than absence.
        for error in [
            "Failed to spawn gh: Too many open files (os error 24)",
            "Failed to spawn gh: Argument list too long (os error 7)",
            "Failed to spawn gh: Resource temporarily unavailable (os error 35)",
        ] {
            assert!(!is_spawn_not_found(error), "{error}");
            assert!(
                matches!(
                    classify_tool_probe("gh", Err(error.to_string().into())),
                    ToolProbe::NotRun(_)
                ),
                "{error}"
            );
        }

        // The forced-failure seam the gh regression test uses is a spawn
        // refusal too, through the real gate.
        let forced = with_forced_spawn_failure(|| {
            capture_command("gh", &["--version"], None, Duration::from_secs(5), &[])
        });
        assert!(matches!(
            classify_tool_probe("gh", forced),
            ToolProbe::NotRun(_)
        ));

        // Started and then ran out of time: found, and failed.
        let hung = format!("gh{TIMEOUT_MARKER}10s");
        assert!(matches!(
            classify_tool_probe("gh", Err(hung.into())),
            ToolProbe::FoundButFailed(_)
        ));
    }

    /// The formatter in `run_bounded` and the matcher behind `run_captured`
    /// share TIMEOUT_MARKER; this pins the round trip so neither side can
    /// drift without breaking here first, even on a platform where the
    /// end-to-end timeout test above does not run. Production always formats
    /// with `label == program`, and the starts_with guard rejects errors
    /// from any other program.
    #[test]
    fn timeout_marker_round_trips_between_formatter_and_matcher() {
        let formatted = format!("sleep{TIMEOUT_MARKER}3s");
        assert!(is_timeout_error("sleep", &formatted));
        // Same contract when the program is spelled as a path: the label is
        // then the full path too, so the prefix still matches.
        let path_formatted = format!("/usr/bin/sleep{TIMEOUT_MARKER}3s");
        assert!(is_timeout_error("/usr/bin/sleep", &path_formatted));
        // A spawn failure with unrelated text must not classify as a timeout…
        assert!(!is_timeout_error(
            "sleep",
            "Failed to spawn sleep: No such file"
        ));
        // …and another program's timeout must not match either.
        assert!(!is_timeout_error("git", &formatted));
    }

    use crate::test_support::git_in;

    fn init_linked_worktree() -> (tempfile::TempDir, tempfile::TempDir, PathBuf) {
        let main = init_test_repo(false);
        git_in(main.path(), &["commit", "--allow-empty", "-m", "init"]);
        let work_parent = tempfile::TempDir::new().unwrap();
        let work_path = work_parent.path().join("linked");
        let output = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
            ])
            .args(["worktree", "add", "-b", "gitpulse-link"])
            .arg(&work_path)
            .current_dir(main.path())
            .output_locked()
            .expect("spawn git worktree");
        assert!(
            output.status.success(),
            "git worktree add failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            work_path.join(".git").is_file(),
            "linked worktree must use a gitfile"
        );
        crate::test_support::trust_repo(&work_path);
        (main, work_parent, work_path)
    }

    fn init_test_repo(bare: bool) -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        let mut cmd = std::process::Command::new("git");
        cmd.arg("init");
        if bare {
            cmd.arg("--bare");
        } else {
            cmd.args(["-b", "main"]);
        }
        let output = cmd
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        crate::test_support::trust_repo(dir.path());
        dir
    }

    #[test]
    fn test_validate_repo_accepts_normal_repo() {
        let dir = init_test_repo(false);
        let canonical = validate_repo(&dir.path().to_string_lossy()).expect("normal repo");
        assert!(canonical.is_absolute());
        assert!(canonical.join(".git").exists());
    }

    #[test]
    fn test_validate_repo_accepts_gitfile() {
        let (_main, _parent, linked) = init_linked_worktree();
        validate_repo(linked.to_str().unwrap()).expect("gitfile worktree");
    }

    #[test]
    fn test_validate_repo_rejects_file_and_incomplete_bare_layout() {
        let dir = tempfile::TempDir::new().unwrap();
        let file = dir.path().join("not-a-dir");
        std::fs::write(&file, "blob").unwrap();
        assert!(validate_repo(&file.to_string_lossy()).is_err());

        let head_only = tempfile::TempDir::new().unwrap();
        std::fs::write(head_only.path().join("HEAD"), "ref: refs/heads/main\n").unwrap();
        assert!(
            validate_repo(&head_only.path().to_string_lossy()).is_err(),
            "HEAD without objects is not a bare repo"
        );

        let objects_only = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(objects_only.path().join("objects")).unwrap();
        assert!(
            validate_repo(&objects_only.path().to_string_lossy()).is_err(),
            "objects without HEAD is not a bare repo"
        );
    }

    #[test]
    fn test_resolve_repo_normal_is_not_bare() {
        let dir = init_test_repo(false);
        let resolved = resolve_repo(&dir.path().to_string_lossy()).expect("resolve");
        assert!(!resolved.is_bare);
        assert_eq!(
            resolved.path,
            dir.path().canonicalize().unwrap().to_string_lossy()
        );
        assert!(!resolved.name.is_empty());
    }

    #[test]
    fn test_validate_repo_accepts_bare_repo() {
        let dir = init_test_repo(true);
        let canonical = validate_repo(&dir.path().to_string_lossy()).expect("bare repo");
        assert!(canonical.join("HEAD").is_file());
        assert!(canonical.join("objects").is_dir());
        assert!(!canonical.join(".git").exists());
        let resolved = resolve_repo(&dir.path().to_string_lossy()).expect("resolve");
        assert!(resolved.is_bare);
        assert_eq!(resolved.path, canonical.to_string_lossy());
        assert!(!resolved.name.is_empty());
    }

    #[test]
    fn resolve_repo_names_one_common_dir_for_every_checkout_of_a_repository() {
        let (main, _work_parent, work_path) = init_linked_worktree();
        let primary = resolve_repo(&main.path().to_string_lossy()).expect("resolve primary");
        let linked = resolve_repo(&work_path.to_string_lossy()).expect("resolve linked");
        let expected = main
            .path()
            .canonicalize()
            .unwrap()
            .join(".git")
            .to_string_lossy()
            .into_owned();
        assert_eq!(primary.common_dir.as_deref(), Some(expected.as_str()));
        assert_eq!(
            linked.common_dir, primary.common_dir,
            "a linked worktree belongs to the repository it was added from"
        );
        assert_ne!(linked.path, primary.path);

        let other = init_test_repo(false);
        let unrelated = resolve_repo(&other.path().to_string_lossy()).expect("resolve other");
        assert_ne!(unrelated.common_dir, primary.common_dir);

        let bare = init_test_repo(true);
        let resolved = resolve_repo(&bare.path().to_string_lossy()).expect("resolve bare");
        assert_eq!(resolved.common_dir.as_deref(), Some(resolved.path.as_str()));
    }

    #[test]
    fn test_validate_repo_rejects_non_repo_directory() {
        let dir = tempfile::TempDir::new().unwrap();
        assert!(validate_repo(&dir.path().to_string_lossy()).is_err());
    }

    #[test]
    fn test_resolve_git_dir_normal_repo() {
        let dir = init_test_repo(false);
        let canonical = validate_repo(&dir.path().to_string_lossy()).unwrap();
        let git_dir = resolve_git_dir(&canonical).expect("git dir");
        assert!(git_dir.ends_with(".git"));
        assert!(git_dir.is_dir());
    }

    #[test]
    fn test_resolve_git_dir_bare_repo() {
        let dir = init_test_repo(true);
        let canonical = validate_repo(&dir.path().to_string_lossy()).unwrap();
        let git_dir = resolve_git_dir(&canonical).expect("git dir");
        assert_eq!(git_dir, canonical);
    }

    #[test]
    fn test_gitfile_worktree_resolves() {
        let (_main, _work_parent, work_path) = init_linked_worktree();
        let raw = work_path.to_string_lossy().into_owned();
        let canonical = validate_repo(&raw).expect("validate gitfile worktree");
        assert!(canonical.join(".git").is_file());

        let resolved = resolve_repo(&raw).expect("resolve gitfile worktree");
        assert!(!resolved.is_bare);
        assert_eq!(resolved.path, canonical.to_string_lossy());
        assert_eq!(resolved.name, "linked");

        let git_dir = resolve_git_dir(&canonical).expect("gitfile git dir");
        assert!(git_dir.is_dir());
        assert_ne!(git_dir, canonical);
        let git_dir_str = git_dir.to_string_lossy();
        assert!(
            git_dir_str.contains("worktrees"),
            "gitfile worktree git-dir should be the worktrees entry, got {git_dir_str}"
        );

        let nested = canonical.join("src").join("lib.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "fn main() {}").unwrap();
        let found = find_git_root(&nested).expect("nested file in gitfile worktree");
        assert_eq!(found, canonical);
    }

    #[test]
    fn test_find_git_root_from_nested_file() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        let nested = dir.path().join("src").join("lib.rs");
        std::fs::create_dir_all(nested.parent().unwrap()).unwrap();
        std::fs::write(&nested, "fn main() {}").unwrap();

        let found = find_git_root(&nested).expect("nested file should resolve to repo");
        assert_eq!(found, dir.path().canonicalize().unwrap());
        assert_eq!(
            find_git_root(dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    #[test]
    fn test_find_git_root_accepts_bare_repo() {
        let dir = init_test_repo(true);
        let found = find_git_root(dir.path()).expect("bare repo");
        assert_eq!(found, dir.path().canonicalize().unwrap());
    }

    #[test]
    fn test_find_git_root_rejects_non_repo() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::write(dir.path().join("readme.txt"), "no git here").unwrap();
        assert!(find_git_root(dir.path()).is_none());
        assert!(find_git_root(&dir.path().join("readme.txt")).is_none());
        assert!(find_git_root(Path::new("")).is_none());
    }

    #[test]
    fn capture_command_keeps_stdout_on_nonzero() {
        let output = capture_command(
            "sh",
            &["-c", "echo findings; echo err >&2; exit 1"],
            None,
            Duration::from_secs(5),
            &[],
        )
        .expect("spawn sh");
        assert!(!output.success);
        assert_eq!(output.status_code, 1);
        assert!(output.stdout_text().contains("findings"));
        assert!(output.stderr_text().contains("err"));
    }

    /// Regression: the stdin payload must be fed from its own thread while
    /// stdout/stderr drain concurrently. A child that emits 256 KiB on stdout
    /// BEFORE reading stdin fills both 64 KiB pipe buffers, and a parent that
    /// writes its >128 KiB payload inline first would block in `write_all`
    /// forever — before the deadline loop ever runs. The watchdog here is the
    /// proof: pre-fix this call never returns and the channel times out.
    #[test]
    fn stdin_write_cannot_deadlock_behind_a_chatty_child() {
        let payload = vec![b'x'; 192 * 1024];
        let (tx, rx) = std::sync::mpsc::channel();
        let started = Instant::now();
        thread::spawn(move || {
            // Writes 256 KiB to stdout first, then consumes all of stdin.
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "head -c 262144 /dev/zero; cat > /dev/null"]);
            let result = run_bounded(cmd, "sh", Duration::from_secs(60), Some(&payload));
            let _ = tx.send(result);
        });
        match rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(out)) => {
                assert_eq!(out.stdout.len(), 262_144);
                assert!(out.success);
            }
            Ok(other) => panic!("expected success, got: {:?}", other.map(|o| o.success)),
            Err(_) => panic!(
                "stdin write deadlocked behind child stdout; no result within 10s \
                 (write_all blocked before the deadline loop could start)"
            ),
        }
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
            "bounded run should finish promptly once pipes drain"
        );
    }

    #[test]
    fn run_command_still_fails_on_nonzero() {
        let err = run_command(
            "sh",
            &["-c", "echo boom >&2; exit 2"],
            Duration::from_secs(5),
        )
        .expect_err("nonzero");
        assert!(err.contains("boom"));
    }

    /// Regression: a GUI launch (Finder/Dock/ScholarLM) hands the app a minimal
    /// PATH without `/opt/homebrew/bin`, so bare names like `gh` failed to
    /// resolve and every GitHub view reported the CLI as "not installed". With
    /// an empty PATH the resolver must fall back to the conventional install
    /// directories (`~/.local/bin` here, standing in for Homebrew's).
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_finds_tool_in_fallback_dir_when_path_is_empty() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("gitpulse-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let resolved = resolve_spawn_program_with(
            "gitpulse-fake-tool",
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            &tool,
            "bare name must resolve through the fallback dir on an empty PATH"
        );
    }

    /// An explicit PATH is the whole search path: the same tool in the same
    /// fallback dir stays unresolved, and the child is handed that PATH
    /// without the fallback dirs, so neither the spawn nor anything the child
    /// re-resolves can reach past it.
    #[cfg(unix)]
    #[test]
    fn an_explicit_path_is_not_widened_by_fallback_dirs() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("gitpulse-fake-tool");
        std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        assert_eq!(
            resolve_spawn_program_in(
                "gitpulse-fake-tool",
                Some(std::ffi::OsStr::new("/usr/bin:/bin")),
                Some(home.path().as_os_str()),
                PathSource::Explicit,
            ),
            "gitpulse-fake-tool",
            "a tool outside the explicit PATH must not resolve"
        );

        let cmd = build_capture_command_in(
            "gitpulse-fake-tool",
            &[],
            None,
            &[],
            Some(std::ffi::OsStr::new("/usr/bin::/bin")),
            Some(home.path().as_os_str()),
            PathSource::Explicit,
        );
        assert_eq!(
            cmd.get_program(),
            std::ffi::OsStr::new("gitpulse-fake-tool")
        );
        let child_path: Vec<Option<std::ffi::OsString>> = cmd
            .get_envs()
            .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .map(|(_, value)| value.map(std::ffi::OsStr::to_os_string))
            .collect();
        assert_eq!(
            child_path,
            vec![Some(std::ffi::OsString::from("/usr/bin:/bin"))],
            "the child gets the explicit PATH, empty entries dropped, nothing appended"
        );

        // No explicit value at all is not "inherit the process PATH".
        let unset = build_capture_command_in(
            "sh",
            &[],
            None,
            &[],
            None,
            Some(home.path().as_os_str()),
            PathSource::Explicit,
        );
        assert!(
            unset
                .get_envs()
                .any(|(key, value)| key == std::ffi::OsStr::new("PATH") && value.is_none()),
            "an explicit unset PATH must be removed from the child"
        );
    }

    /// Git children must carry the extended PATH, with inherited entries
    /// still ahead of the appended ones so nothing that already resolved
    /// starts resolving somewhere else.
    #[cfg(unix)]
    #[test]
    fn git_command_hands_children_the_extended_path() {
        let home = tempfile::TempDir::new().unwrap();
        let cmd = git_command_with_env(
            None,
            &["status"],
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        );
        let path = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("git children must be handed an extended PATH");
        let entries: Vec<PathBuf> = std::env::split_paths(path).collect();
        assert_eq!(
            &entries[..2],
            [Path::new("/usr/bin"), Path::new("/bin")],
            "inherited entries must keep their precedence: {entries:?}"
        );
        for fallback in [
            PathBuf::from("/opt/homebrew/bin"),
            home.path().join(".cargo/bin"),
        ] {
            assert!(
                entries.contains(&fallback),
                "child PATH must reach {}: {entries:?}",
                fallback.display()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn git_launch_resolves_the_child_path_without_loss_or_search_order_changes() {
        use std::os::unix::{ffi::OsStringExt, fs::PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        // APFS rejects invalid UTF-8 names; Unix filesystems that support
        // them exercise byte-preserving executable resolution as well.
        let name = if cfg!(target_os = "macos") {
            std::ffi::OsString::from("first-é")
        } else {
            std::ffi::OsString::from_vec(b"first-\xff".to_vec())
        };
        let first = root.path().join(name);
        let second = root.path().join("second");
        for dir in [&first, &second] {
            std::fs::create_dir(dir).unwrap();
            let tool = dir.join("git");
            std::fs::write(&tool, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = std::env::join_paths([&first, &second]).unwrap();
        let cmd = git_command_with_env(Some(root.path()), &["status"], Some(&path), None);
        assert_eq!(cmd.get_program(), first.join("git"));
        assert!(
            Path::new(cmd.get_program()).is_absolute(),
            "a changed PATH plus a bare program forces Rust to fork"
        );
        std::fs::set_permissions(first.join("git"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        let cmd = git_command_with_env(None, &["status"], Some(&path), None);
        assert_eq!(cmd.get_program(), second.join("git"));
    }

    /// Regression: a Finder-launched GitPulse inherits `/usr/bin:/bin:...`,
    /// so every git read was spawned through Apple's `xcrun` shim at more
    /// than twice the cost of the binary it execs, even with Homebrew's git
    /// installed. Driven through the real launch path on this host.
    #[cfg(target_os = "macos")]
    #[test]
    fn git_launch_never_spawns_through_the_xcrun_shim_when_a_real_git_exists() {
        let shim = PathBuf::from(XCRUN_GIT_SHIM);
        let real_exists = past_xcrun_git_shim(
            shim.clone(),
            &REAL_GIT_DIRS.map(PathBuf::from),
            &developer_dirs(),
        ) != shim;
        let gui_path = std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin");
        let cmd = git_command_with_env(None, &["--version"], Some(gui_path), None);
        let program = Path::new(cmd.get_program());
        if real_exists {
            assert_ne!(
                program,
                Path::new(XCRUN_GIT_SHIM),
                "spawned through the shim"
            );
            let out = std::process::Command::new(program)
                .arg("--version")
                .output_locked()
                .expect("the chosen git runs");
            assert!(out.status.success());
            assert!(String::from_utf8_lossy(&out.stdout).starts_with("git version "));
        } else {
            assert_eq!(program, Path::new(XCRUN_GIT_SHIM));
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn xcrun_shim_bypass_prefers_packaged_then_developer_git_and_nothing_else() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let exe = |path: &Path| {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        let shim = PathBuf::from(XCRUN_GIT_SHIM);
        let brew = root.join("brew");
        let local = root.join("local");
        let xcode = root.join("Xcode.app");
        let clt = root.join("CommandLineTools");
        let packaged = [brew.clone(), local.clone()];

        // Nothing real installed: the shim stays, since it is what offers to
        // install the developer tools.
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, std::slice::from_ref(&clt)),
            shim
        );

        // Developer tools only; `DEVELOPER_DIR` may name the bundle itself.
        exe(&xcode.join("Contents/Developer/usr/bin/git"));
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, &[xcode.clone(), clt.clone()]),
            xcode.join("Contents/Developer/usr/bin/git")
        );
        exe(&clt.join("usr/bin/git"));
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, &[clt.clone(), xcode.clone()]),
            clt.join("usr/bin/git"),
            "developer dirs are tried in the order xcrun consults them"
        );

        // A packaged git outranks the developer tools, in directory order.
        exe(&local.join("git"));
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, std::slice::from_ref(&clt)),
            local.join("git")
        );
        exe(&brew.join("git"));
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, std::slice::from_ref(&clt)),
            brew.join("git")
        );

        // A git the user put ahead of /usr/bin on PATH is never second-guessed.
        let chosen = root.join("chosen/git");
        assert_eq!(
            past_xcrun_git_shim(chosen.clone(), &packaged, std::slice::from_ref(&clt)),
            chosen
        );

        // Not executable, a directory, or a link back to the shim: skipped.
        std::fs::set_permissions(brew.join("git"), std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::remove_file(local.join("git")).unwrap();
        std::fs::create_dir(local.join("git")).unwrap();
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, std::slice::from_ref(&clt)),
            clt.join("usr/bin/git")
        );
        std::fs::remove_file(brew.join("git")).unwrap();
        symlink(&shim, brew.join("git")).unwrap();
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &packaged, std::slice::from_ref(&clt)),
            clt.join("usr/bin/git"),
            "a link to the shim costs what the shim does"
        );

        // Relative entries resolve against the cwd, so they are never used.
        let relative = [PathBuf::from("relative")];
        assert_eq!(
            past_xcrun_git_shim(shim.clone(), &relative, &[PathBuf::from("rel")]),
            shim
        );
    }

    #[cfg(unix)]
    #[test]
    fn git_launch_drops_relative_path_entries_and_unset_path_defaults() {
        // A relative PATH entry has child-cwd semantics, and the child's cwd
        // is the repository: it is dropped rather than honoured, so git is
        // resolved from the absolute entries and never from inside the repo.
        let cmd = git_command_with_env(
            None,
            &["status"],
            Some(std::ffi::OsStr::new("relative:/usr/bin")),
            None,
        );
        assert!(Path::new(cmd.get_program()).is_absolute());
        let cmd = git_command_with_env(None, &["status"], None, None);
        assert!(Path::new(cmd.get_program()).is_absolute());
    }

    /// Regression, driven through the failure users actually hit: git resolves
    /// its own helpers through the CHILD's PATH — `gpg` for a signed commit,
    /// the interpreter a `pre-commit` hook shells out to, `git-lfs`, external
    /// diff/merge tools. A GUI launch handed them the minimal
    /// `/usr/bin:/bin:/usr/sbin:/sbin`, so a husky-style hook exited 127 and a
    /// signed commit failed with "cannot run gpg" on machines where both were
    /// installed — reading as a broken repository rather than a PATH we chose.
    ///
    /// A real hook rather than an env assertion: the point is that git's own
    /// child found a tool that exists in nothing but a fallback directory.
    #[cfg(unix)]
    #[test]
    fn git_hooks_resolve_helpers_that_live_only_in_a_fallback_dir() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let helper = bin.join("gitpulse-fake-hook-helper");
        std::fs::write(&helper, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&helper, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let repo = init_test_repo(false);
        let hooks = repo.path().join(".git/hooks");
        std::fs::create_dir_all(&hooks).unwrap();
        let hook = hooks.join("pre-commit");
        // Bare name on purpose: resolving it is the whole assertion.
        std::fs::write(&hook, "#!/bin/sh\nexec gitpulse-fake-hook-helper\n").unwrap();
        std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // Identity and signing are pinned inline because the child keeps the
        // real HOME: a developer's global `commit.gpgsign` must not decide
        // whether this passes, and signing would block on a passphrase.
        let mut cmd = git_command_with_env(
            Some(repo.path()),
            &[
                "-c",
                "user.name=GitPulse",
                "-c",
                "user.email=gitpulse@test.local",
                "-c",
                "commit.gpgsign=false",
                "commit",
                "--allow-empty",
                "-m",
                "hook must run",
            ],
            Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            Some(home.path().as_os_str()),
        );
        let out = cmd.output_locked().expect("spawn git");
        assert!(
            out.status.success(),
            "pre-commit hook could not resolve a fallback-dir helper: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// Regression: the same GUI-launch miss as the `gh` case above, one
    /// directory short. `rustup` installs the whole Rust toolchain — `cargo`,
    /// `rustc`, `rustup` itself, and cargo subcommand binaries such as
    /// `cargo-audit` and `cargo-llvm-cov` — into `~/.cargo/bin`, which is on
    /// no GUI-launch PATH and was in no fallback dir. Running `cargo` from the
    /// terminal panel died with "Failed to spawn cargo: No such file or
    /// directory (os error 2)", the dependency scanner reported the Rust
    /// ecosystem as unscannable, and the coverage panel reported
    /// `cargo llvm-cov` absent — on machines where all of it was installed.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_finds_rust_toolchain_in_cargo_bin() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();

        // Half the regression: the directory has to be in the list at all.
        // Every Rust entry point the app spawns by bare name — `cargo`,
        // `rustc`, `rustup`, and `cargo-*` subcommands like `cargo-audit` and
        // `cargo-llvm-cov` — lives in this one directory, so its absence took
        // all of them out together. Asserting the list directly is the only
        // part of that claim the host cannot influence.
        assert!(
            gui_launch_fallback_dirs(Some(home.path().as_os_str())).contains(&bin),
            "~/.cargo/bin must be a GUI-launch fallback directory"
        );

        // The other half: resolution has to walk far enough to reach it. It is
        // the LAST entry, behind real system directories (`/opt/homebrew/bin`,
        // `/usr/local/bin`) that this test cannot sandbox — so probing with a
        // real tool name measures the host, not the code. A macOS runner with
        // a Homebrew `rustup` resolved `/opt/homebrew/bin/rustup`, which is
        // the correct answer on that machine, and failed a test that meant to
        // ask something else. A name no machine can have crosses every earlier
        // directory and can only be found in the last one.
        let probe = "gitpulse-cargo-bin-probe";
        let path = bin.join(probe);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // The GUI-launch PATH verbatim: this is what the bundled app gets from
        // launchd, and it is why the fallback list has to carry the directory.
        let resolved = resolve_spawn_program_with(
            probe,
            Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            &path,
            "resolution must reach ~/.cargo/bin on a GUI-launch PATH"
        );
    }

    #[cfg(unix)]
    #[test]
    fn spawn_resolution_finds_grok_in_its_install_dir() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".grok/bin");
        std::fs::create_dir_all(&bin).unwrap();
        assert!(
            gui_launch_fallback_dirs(Some(home.path().as_os_str())).contains(&bin),
            "~/.grok/bin must be a GUI-launch fallback directory"
        );
        let probe = "gitpulse-grok-bin-probe";
        let path = bin.join(probe);
        std::fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();
        let resolved = resolve_spawn_program_with(
            probe,
            Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            &path,
            "resolution must reach ~/.grok/bin on a GUI-launch PATH"
        );
    }

    /// A fallback directory that does not depend on the user's home must not
    /// be gated on knowing it. `/usr/local/go/bin` is a fixed system path, so
    /// dropping it when `home` is unset (a launchd/daemon context, where this
    /// mechanism matters most) withheld a directory that was still valid.
    #[cfg(unix)]
    #[test]
    fn gui_fallback_keeps_system_dirs_when_home_is_unknown() {
        let dirs = gui_launch_fallback_dirs(None);
        for system_dir in [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/usr/local/go/bin"),
        ] {
            assert!(
                dirs.contains(&system_dir),
                "home-independent dir must survive an unknown home: {} in {dirs:?}",
                system_dir.display()
            );
        }
        // Home-relative dirs are the only ones an unknown home may cost.
        assert!(
            dirs.iter().all(|d| d.is_absolute()),
            "no relative entry may reach the search list: {dirs:?}"
        );
    }

    /// PATH order wins: a name present on the inherited PATH must not be
    /// shadowed by a fallback-directory copy. Both candidates are executable
    /// so the strict `find_in_dirs` scan considers them at all.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_prefers_path_over_fallback_dirs() {
        let path_dir = tempfile::TempDir::new().unwrap();
        let home_dir = tempfile::TempDir::new().unwrap();
        for dir in [path_dir.path(), &home_dir.path().join(".local/bin")] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("gitpulse-shadowed"), "#!/bin/sh\nexit 0\n").unwrap();
            std::fs::set_permissions(
                dir.join("gitpulse-shadowed"),
                std::os::unix::fs::PermissionsExt::from_mode(0o755),
            )
            .unwrap();
        }
        let resolved = resolve_spawn_program_with(
            "gitpulse-shadowed",
            Some(path_dir.path().as_os_str()),
            Some(home_dir.path().as_os_str()),
        );
        assert_eq!(
            Path::new(&resolved),
            path_dir.path().join("gitpulse-shadowed")
        );
    }

    /// A non-executable file must not win the lookup: `execvp` skips it and
    /// keeps searching, so an executable copy later in the search order has to
    /// be picked instead of failing the eventual spawn with PermissionDenied.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_skips_non_executable_candidate_for_later_match() {
        let first = tempfile::TempDir::new().unwrap();
        let second = tempfile::TempDir::new().unwrap();
        std::fs::write(first.path().join("gitpulse-exec-probe"), "data, not code").unwrap();
        let good = second.path().join("gitpulse-exec-probe");
        std::fs::write(&good, "#!/bin/sh\nexit 0\n").unwrap();
        std::fs::set_permissions(&good, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let path_var = std::env::join_paths([first.path(), second.path()]).expect("join dirs");
        let resolved = resolve_spawn_program_with("gitpulse-exec-probe", Some(&path_var), None);
        assert_eq!(
            Path::new(&resolved),
            &good,
            "executable copy in a later dir must beat a non-executable earlier one"
        );
    }

    /// Empty PATH entries mean CWD (POSIX) and relative entries resolve
    /// against whatever cwd the child ends up with — both must be ignored by
    /// the resolver even when they really contain the tool.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_ignores_empty_and_relative_path_entries() {
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let rel = Cleanup(PathBuf::from("gitpulse-rel-probe-dir"));
        std::fs::create_dir_all(&rel.0).unwrap();
        std::fs::write(rel.0.join("gitpulse-rel-probe-tool"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            rel.0.join("gitpulse-rel-probe-tool"),
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();

        for hostile in ["", "::", "gitpulse-rel-probe-dir"] {
            let resolved = resolve_spawn_program_with(
                "gitpulse-rel-probe-tool",
                Some(std::ffi::OsStr::new(hostile)),
                None,
            );
            assert_eq!(
                resolved, "gitpulse-rel-probe-tool",
                "entry {hostile:?} must not resolve a relative candidate"
            );
        }
    }

    /// Broken symlinks and directories that merely share the tool's name are
    /// not spawnpable candidates.
    #[cfg(unix)]
    #[test]
    fn spawn_resolution_skips_broken_symlink_and_directory_named_like_tool() {
        let dir = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(
            "/definitely/does/not/exist",
            dir.path().join("gitpulse-dangling"),
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("gitpulse-dirname")).unwrap();

        for name in ["gitpulse-dangling", "gitpulse-dirname"] {
            let resolved = resolve_spawn_program_with(name, Some(dir.path().as_os_str()), None);
            assert_eq!(
                resolved, name,
                "{name} must be skipped rather than selected as a candidate"
            );
        }
    }

    /// The child PATH never carries CWD-searching empty entries, and a fully
    /// absent PATH is seeded with the Unix default set instead of leaving
    /// children with only Homebrew/fallback dirs.
    #[cfg(unix)]
    #[test]
    fn extended_child_path_drops_cwd_entries_and_seeds_default_when_unset() {
        let home = tempfile::TempDir::new().unwrap();
        let colon_joined = extended_child_path(
            Some(std::ffi::OsStr::new("::")),
            Some(home.path().as_os_str()),
        )
        .expect("join");
        let entries: Vec<PathBuf> = std::env::split_paths(&colon_joined).collect();
        assert!(
            entries.iter().all(|e| !e.as_os_str().is_empty()),
            "empty CWD entries must be dropped: {entries:?}"
        );
        assert_eq!(
            entries.first(),
            Some(&PathBuf::from("/opt/homebrew/bin")),
            "with nothing inherited, fallbacks start immediately"
        );

        let unset = extended_child_path(None, Some(home.path().as_os_str())).expect("join");
        let seeded: Vec<PathBuf> = std::env::split_paths(&unset).collect();
        assert_eq!(
            &seeded[..2],
            [Path::new("/usr/bin"), Path::new("/bin")],
            "unset PATH must seed the Unix default search set"
        );
    }

    /// A relative entry resolves against the child's cwd, which is the repo.
    #[cfg(unix)]
    #[test]
    fn extended_child_path_drops_relative_entries() {
        let home = tempfile::TempDir::new().unwrap();
        let hostile = extended_child_path(
            Some(std::ffi::OsStr::new(
                "./node_modules/.bin:.:bin:/usr/local/bin:../x:node_modules/.bin",
            )),
            Some(home.path().as_os_str()),
        )
        .expect("join");
        let entries: Vec<PathBuf> = std::env::split_paths(&hostile).collect();
        assert!(
            entries.iter().all(|e| e.is_absolute()),
            "relative PATH entries must be dropped: {entries:?}"
        );
        assert_eq!(entries.first(), Some(&PathBuf::from("/usr/local/bin")));
    }

    /// The extended child PATH appends only the fallback dirs that are not
    /// already present, preserving inherited order and precedence.
    #[cfg(unix)]
    /// `argv` and `envp` share one buffer and `execve` refuses the whole spawn
    /// once it is full. Appending six directories to a PATH already at that
    /// ceiling is how a helpful change becomes the reason a working spawn
    /// stops working — surfacing as "Failed to spawn git: Argument list too
    /// long", with nothing pointing back here.
    ///
    /// The ceiling is measured on the host rather than taken from a manual,
    /// and the guard is asserted to sit below it with room to spare: a guard
    /// at or above the real limit protects nothing while looking like it does.
    #[test]
    #[cfg(unix)]
    fn a_path_at_the_exec_ceiling_is_never_grown() {
        let home = tempfile::TempDir::new().unwrap();
        let entry = "/".repeat(1000);

        let under = std::iter::repeat_n(entry.as_str(), (MAX_INHERITED_PATH_BYTES / 1001) - 1)
            .collect::<Vec<_>>()
            .join(":");
        assert!(
            under.len() < MAX_INHERITED_PATH_BYTES,
            "fixture must sit under the guard"
        );
        let extended = extended_child_path(
            Some(std::ffi::OsStr::new(&under)),
            Some(home.path().as_os_str()),
        )
        .expect("a PATH under the guard must still be extended");
        assert!(
            extended.len() > under.len(),
            "the fallback dirs were not appended below the guard"
        );

        for size in [
            MAX_INHERITED_PATH_BYTES,
            MAX_INHERITED_PATH_BYTES * 2,
            MAX_INHERITED_PATH_BYTES * 16,
        ] {
            assert!(
                extended_child_path(
                    Some(std::ffi::OsStr::new(&"x".repeat(size))),
                    Some(home.path().as_os_str())
                )
                .is_none(),
                "a {size}-byte PATH was grown toward E2BIG"
            );
        }

        let mut refused_at = None;
        for bytes in [64 * 1024, 256 * 1024, 512 * 1024, 1024 * 1024, 4096 * 1024] {
            let refused = Command::new("/usr/bin/true")
                .env_clear()
                .env("PATH", "x".repeat(bytes))
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status_locked()
                .is_err();
            if refused {
                refused_at = Some(bytes);
                break;
            }
        }
        match refused_at {
            Some(ceiling) => assert!(
                MAX_INHERITED_PATH_BYTES < ceiling,
                "the guard ({MAX_INHERITED_PATH_BYTES}) is at or above this host's measured exec \
                 ceiling ({ceiling}), so it protects nothing"
            ),
            // A host with no reachable ceiling cannot falsify the guard.
            // Saying so is the honest outcome; passing quietly would make an
            // unmeasured host read like a measured one.
            None => eprintln!("exec accepted a 4 MiB PATH: no ceiling in range, guard unverified"),
        }
    }

    /// Extending must never hand a child a *smaller* search path.
    ///
    /// A dropped entry silently relocates which `git`, `gh` or `devmap` the
    /// child resolves — the same wrong-binary class the installation-health
    /// report exists to catch, introduced by the code that reports it.
    #[test]
    #[cfg(unix)]
    fn extending_never_removes_an_inherited_entry() {
        let home = tempfile::TempDir::new().unwrap();
        for case in [
            "/usr/bin:/bin",
            // Empty entries mean "the current directory" to POSIX and are
            // dropped deliberately; everything else must survive.
            "/usr/bin::/bin:",
            "/opt/homebrew/bin:/usr/bin",
            "/a b/bin:/c\td/bin",
            "/usr/bin:/usr/bin:/usr/bin",
            "/ünïcode/bin:/usr/bin",
        ] {
            let extended = extended_child_path(
                Some(std::ffi::OsStr::new(case)),
                Some(home.path().as_os_str()),
            )
            .unwrap_or_else(|| panic!("join failed for {case}"));
            let before: Vec<PathBuf> = std::env::split_paths(std::ffi::OsStr::new(case))
                .filter(|entry| !entry.as_os_str().is_empty())
                .collect();
            let after: Vec<PathBuf> = std::env::split_paths(&extended).collect();
            for entry in &before {
                assert!(
                    after.contains(entry),
                    "{} vanished from the child PATH for {case}",
                    entry.display()
                );
            }
            assert_eq!(
                after[..before.len()],
                before[..],
                "inherited precedence changed for {case}"
            );
        }
    }

    /// Resolving a tool and running it are two different PATH questions.
    ///
    /// GitPulse finds `devmap` in `~/.local/bin` through the GUI fallback dirs
    /// and then, before this existed, spawned it with launchd's minimal
    /// `/usr/bin:/bin:/usr/sbin:/sbin`. `devmap doctor` resolves the bare
    /// `devmap` command named by host MCP configs against *its* PATH, found
    /// nothing, and reported "host MCP config names a devmap path that is not
    /// a file: devmap — this is not version skew". GitPulse rendered that as
    /// the user's broken install. It was our PATH.
    ///
    /// Asserted as set containment against [`gui_launch_fallback_dirs`] rather
    /// than a spelled-out list: a directory added there and not handed to the
    /// child is the same bug again, and a hand-written expectation here would
    /// keep passing through it.
    #[test]
    #[cfg(unix)]
    fn a_spawned_tool_can_see_the_directory_it_was_resolved_from() {
        let home = tempfile::TempDir::new().unwrap();
        let mut cmd = Command::new("/usr/bin/true");
        default_child_path_with_env(
            &mut cmd,
            Some(std::ffi::OsStr::new("/usr/bin:/bin:/usr/sbin:/sbin")),
            Some(home.path().as_os_str()),
        );
        let child_path = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("an absent PATH must be filled in")
            .to_owned();
        let entries: Vec<PathBuf> = std::env::split_paths(&child_path).collect();
        for fallback in gui_launch_fallback_dirs(Some(home.path().as_os_str())) {
            assert!(
                entries.contains(&fallback),
                "child cannot see {}: {entries:?}",
                fallback.display()
            );
        }
        // Inherited entries keep their precedence: a fallback dir must never
        // shadow a tool the user deliberately put earlier on PATH.
        assert_eq!(entries[0], PathBuf::from("/usr/bin"));
    }

    /// The caller's own PATH is a decision, not an omission.
    ///
    /// `build_capture_command`, the PTY and the harness sidecar each set one;
    /// re-deriving it from this process would quietly discard the environment
    /// the user is actually running in.
    #[test]
    #[cfg(unix)]
    fn a_path_the_caller_chose_is_never_replaced() {
        let home = tempfile::TempDir::new().unwrap();
        let mut cmd = Command::new("/usr/bin/true");
        cmd.env("PATH", "/caller/chosen");
        default_child_path_with_env(
            &mut cmd,
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        );
        let values: Vec<_> = cmd
            .get_envs()
            .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .map(|(_, value)| value.map(std::ffi::OsStr::to_owned))
            .collect();
        assert_eq!(
            values,
            vec![Some(std::ffi::OsString::from("/caller/chosen"))],
            "caller PATH replaced or duplicated"
        );
    }

    /// Setting PATH for a bare program name drops `std` off `posix_spawn` onto
    /// `fork` (`env_saw_path() && !program_is_path()`), and forking a GUI
    /// process this size is both slow and the shape behind the concurrent
    /// descriptor races [`SpawnGate`] exists to bound.
    ///
    /// Bare names are not left unhelped by this: they arrive through
    /// `build_capture_command` or `git_command_with_env`, which resolve the
    /// program to a path *and* set PATH themselves.
    #[test]
    #[cfg(unix)]
    fn a_bare_program_name_keeps_posix_spawn() {
        let home = tempfile::TempDir::new().unwrap();
        let mut cmd = Command::new("true");
        default_child_path_with_env(
            &mut cmd,
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            cmd.get_envs()
                .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
                .count(),
            0,
            "a bare program name must not gain a PATH override"
        );
        // A relative path is still a path, and `std` treats it as one.
        let mut relative = Command::new("./devmap");
        default_child_path_with_env(
            &mut relative,
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        );
        assert_eq!(
            relative
                .get_envs()
                .filter(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
                .count(),
            1,
            "a relative path is a path and must be given the child PATH"
        );
    }

    #[test]
    #[cfg(unix)]
    fn extended_child_path_appends_missing_fallback_dirs_only() {
        let home = tempfile::TempDir::new().unwrap();
        let extended = extended_child_path(
            Some(std::ffi::OsStr::new("/usr/bin:/bin")),
            Some(home.path().as_os_str()),
        )
        .expect("join must succeed for plain entries");
        let entries: Vec<PathBuf> = std::env::split_paths(&extended).collect();
        assert_eq!(entries[0], PathBuf::from("/usr/bin"));
        assert_eq!(entries[1], PathBuf::from("/bin"));
        for fallback in [
            PathBuf::from("/opt/homebrew/bin"),
            PathBuf::from("/usr/local/bin"),
            home.path().join(".local/bin"),
            home.path().join(".grok/bin"),
            // Go toolchain locations so govulncheck's own `go` spawn resolves.
            PathBuf::from("/usr/local/go/bin"),
            home.path().join("go/bin"),
            // Rust toolchain location, for the same nested-lookup reason: a
            // `cargo` resolved here still spawns `rustc` and its own
            // `cargo-*` subcommand binaries out of the child's PATH.
            home.path().join(".cargo/bin"),
        ] {
            assert!(
                entries.contains(&fallback),
                "fallback dir must be appended: {} in {entries:?}",
                fallback.display()
            );
        }

        // Already-present entries are never duplicated.
        let deduped = extended_child_path(
            Some(std::ffi::OsStr::new("/opt/homebrew/bin")),
            Some(home.path().as_os_str()),
        )
        .expect("join must succeed");
        let count = std::env::split_paths(&deduped)
            .filter(|p| p == Path::new("/opt/homebrew/bin"))
            .count();
        assert_eq!(count, 1, "must not duplicate an existing entry");
    }

    /// An explicit caller-provided PATH (via `extra_env`) outranks the child
    /// PATH extension — the extension only fills an absent decision.
    #[test]
    fn extra_env_path_overrides_child_path_extension() {
        let home = tempfile::TempDir::new().unwrap();
        let cmd = build_capture_command(
            "sh",
            &[],
            None,
            &[("PATH", "/caller/chosen")],
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        let path_values: Vec<String> = cmd
            .get_envs()
            .filter(|(k, _)| *k == std::ffi::OsStr::new("PATH"))
            .filter_map(|(_, v)| v.map(|v| v.to_string_lossy().into_owned()))
            .collect();
        assert_eq!(path_values, vec!["/caller/chosen".to_string()]);
    }

    /// Regression (GUI launch + shebang scripts): resolving the top-level
    /// program through the fallback dirs is not enough. `/opt/homebrew/bin/npm`
    /// is a symlink to a `#!/usr/bin/env node` script; under a GUI-minimal
    /// inherited PATH the spawn's own `env` interpreter lookup fails (exit 127,
    /// "env: node: No such file or directory"), which reads downstream as
    /// "npm is not installed". The child must see the fallback dirs on its own
    /// PATH too.
    ///
    /// Asserted on the command GitPulse builds, which is the whole of GitPulse's
    /// side of this contract: the top-level program resolves to the fallback-dir
    /// tool, and the child is handed a PATH on which that tool's `#!/usr/bin/env`
    /// interpreter resolves. Both are decided before anything is spawned, so this
    /// case cannot be lost to how long the host takes to start a process — which
    /// is what the end-to-end sibling below is exposed to.
    #[cfg(unix)]
    #[test]
    fn shebang_tool_in_fallback_dir_is_given_a_child_path_reaching_its_interpreter() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let interpreter = bin.join("gitpulse-fake-interp");
        std::fs::write(&interpreter, "#!/bin/sh\necho INTERP_OK\n").unwrap();
        std::fs::set_permissions(
            &interpreter,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let tool = bin.join("gitpulse-fake-shebang-tool");
        std::fs::write(&tool, "#!/usr/bin/env gitpulse-fake-interp\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        let cmd = build_capture_command(
            "gitpulse-fake-shebang-tool",
            &[],
            None,
            &[],
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );

        // The tool itself was found in the fallback dir, not left as a bare name
        // for an empty PATH to fail on.
        assert_eq!(
            Path::new(cmd.get_program()),
            tool,
            "the program must resolve to the fallback-dir tool"
        );

        // And the child's own PATH reaches the interpreter that tool's shebang
        // names. Resolved by searching, not by string matching: `env` will do a
        // PATH search, so this asserts the thing `env` will actually decide.
        let child_path = cmd
            .get_envs()
            .find(|(key, _)| *key == std::ffi::OsStr::new("PATH"))
            .and_then(|(_, value)| value)
            .expect("the child must be handed a PATH");
        let found = std::env::split_paths(child_path)
            .map(|dir| dir.join("gitpulse-fake-interp"))
            .find(|candidate| candidate.is_file());
        assert_eq!(
            found.as_deref(),
            Some(interpreter.as_path()),
            "child PATH must reach the interpreter: {child_path:?}"
        );
    }

    /// The same contract, proved by running it: `/usr/bin/env` really does
    /// resolve the interpreter through the PATH the sibling test above asserts.
    ///
    /// `#[ignore]`d because its verdict is not about GitPulse. It is bounded by a
    /// 30s deadline on a child that does nothing but `echo`, and children on a
    /// loaded host do not reliably reach `main` inside any such bound: measured
    /// here, spawned stubs that blew a 30s and a 60s deadline went on to exit 0
    /// one and fifty-seven seconds later respectively. As a gate this reported
    /// host load; the assertion it was meant to make is made deterministically
    /// above. Raising the deadline would only make the misreport rarer.
    ///
    /// Run it deliberately after changing spawn resolution or child PATH with
    /// `cargo test --manifest-path src-tauri/Cargo.toml --lib
    /// shebang_tool_in_fallback_dir_finds_interpreter_through_child_path --
    /// --ignored`.
    #[cfg(unix)]
    #[test]
    #[ignore = "bounds a real process spawn on a wall clock; host load, not GitPulse, decides it"]
    fn shebang_tool_in_fallback_dir_finds_interpreter_through_child_path() {
        let home = tempfile::TempDir::new().unwrap();
        let bin = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let interpreter = bin.join("gitpulse-fake-interp");
        std::fs::write(&interpreter, "#!/bin/sh\necho INTERP_OK\n").unwrap();
        std::fs::set_permissions(
            &interpreter,
            std::os::unix::fs::PermissionsExt::from_mode(0o755),
        )
        .unwrap();
        let tool = bin.join("gitpulse-fake-shebang-tool");
        std::fs::write(&tool, "#!/usr/bin/env gitpulse-fake-interp\n").unwrap();
        std::fs::set_permissions(&tool, std::os::unix::fs::PermissionsExt::from_mode(0o755))
            .unwrap();

        // An empty inherited PATH stands in for a Finder/Dock launch: the tool
        // resolves via ~/.local/bin and its interpreter must resolve the same
        // way inside the child.
        let cmd = build_capture_command(
            "gitpulse-fake-shebang-tool",
            &[],
            None,
            &[],
            Some(std::ffi::OsStr::new("")),
            Some(home.path().as_os_str()),
        );
        let out = run_bounded(
            cmd,
            "gitpulse-fake-shebang-tool",
            Duration::from_secs(30),
            None,
        )
        .expect("spawn");
        assert!(
            out.success,
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            String::from_utf8_lossy(&out.stdout).contains("INTERP_OK"),
            "stdout: {}",
            String::from_utf8_lossy(&out.stdout)
        );
    }

    /// Node ships `npm.cmd` and an extension-less `npm` shell script side by
    /// side. Resolving the latter hands `CreateProcess` a non-PE file, which is
    /// where "%1 is not a valid Win32 application" came from.
    #[test]
    fn spawn_resolution_prefers_a_windows_executable_over_a_bare_script() {
        let names = spawn_candidate_names("npm");
        if cfg!(windows) {
            assert_eq!(
                names.last().map(String::as_str),
                Some("npm"),
                "the bare name must be the LAST resort, not the first: {names:?}"
            );
            assert!(
                names.iter().any(|name| name == "npm.cmd"),
                "npm.cmd must be reachable: {names:?}"
            );
            let cmd = names.iter().position(|name| name == "npm.cmd");
            let bare = names.iter().position(|name| name == "npm");
            assert!(cmd < bare, "npm.cmd must be tried before npm: {names:?}");
        } else {
            assert_eq!(names, vec!["npm".to_string()]);
        }
    }

    /// A name that already carries an executable suffix is used as written.
    #[test]
    fn spawn_resolution_never_double_suffixes_an_executable_name() {
        for program in ["gh.exe", "GH.EXE", "thing.cmd"] {
            let names = spawn_candidate_names(program);
            assert_eq!(
                names,
                vec![program.to_string()],
                "{program} must not gain a second suffix"
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn index_transaction_accepts_a_canonical_windows_parent_and_new_index() {
        let dir = tempfile::TempDir::new().unwrap();
        crate::test_support::git_in(dir.path(), &["init"]);
        let canonical = dir.path().canonicalize().unwrap();
        let index = canonical.join(".git").join("transaction-index");
        assert!(!index.exists());
        git_with_index(dir.path(), &index, &["read-tree", "--empty"], b"").unwrap();
        assert!(index.is_file());
        assert!(!dir.path().join(".git/index").exists());
        let paths = git_with_index(dir.path(), &index, &["ls-files", "-z"], b"").unwrap();
        assert!(paths.is_empty());
    }

    /// git refuses a verbatim path as a working tree
    /// ("could not create work tree dir '\\?\C:\...': Invalid argument"), and
    /// canonicalize hands one back for every path on Windows. These are the
    /// shapes it actually produces.
    #[test]
    fn a_verbatim_windows_path_is_rewritten_in_ordinary_form() {
        assert_eq!(
            simplified_windows_path(r"\\?\C:\Users\runneradmin\repo").as_deref(),
            Some(r"C:\Users\runneradmin\repo")
        );
        assert_eq!(
            simplified_windows_path(r"\\?\UNC\server\share\repo").as_deref(),
            Some(r"\\server\share\repo")
        );
    }

    /// Not every verbatim path has an ordinary spelling. Guessing one would
    /// produce a path that names a different file, or no file at all.
    #[test]
    fn a_path_with_no_ordinary_spelling_keeps_its_prefix() {
        for path in [
            r"\\?\Volume{b75e2c83-0000-0000-0000-602f00000000}\repo",
            r"C:\already\ordinary",
            r"\\server\share\already\unc",
            "/unix/style/path",
            r"\\?\",
            r"\\?\C:",
        ] {
            assert_eq!(
                simplified_windows_path(path),
                None,
                "{path} has no ordinary spelling and must keep what it had"
            );
        }
    }

    /// The property the clone path depends on, checked against a real
    /// canonicalize rather than a literal.
    #[cfg(windows)]
    #[test]
    fn canonicalize_plain_answers_in_a_spelling_git_accepts() {
        let dir = tempfile::TempDir::new().unwrap();
        let plain = canonicalize_plain(dir.path()).expect("temp dir canonicalizes");
        assert!(
            !plain.to_string_lossy().starts_with(r"\\?\"),
            "canonicalize_plain still returned a verbatim path: {plain:?}"
        );
        // And it still names the same directory it resolved.
        assert_eq!(
            plain.canonicalize().unwrap(),
            dir.path().canonicalize().unwrap(),
            "the ordinary spelling must denote the same directory"
        );
    }

    /// The text form of [`canonicalize_plain`], for the paths the command gate
    /// and `git worktree add` receive already spelled: a canonicalized checkout
    /// joined with a lane directory.
    #[cfg(windows)]
    #[test]
    fn plain_path_text_drops_the_verbatim_prefix_git_cannot_use() {
        assert_eq!(
            plain_path_text(r"\\?\C:\Users\runneradmin\repo\.gitpulse\worktrees\lane"),
            r"C:\Users\runneradmin\repo\.gitpulse\worktrees\lane"
        );
        assert_eq!(
            plain_path_text(r"\\?\UNC\server\share\repo"),
            r"\\server\share\repo"
        );
        // No ordinary spelling exists: unchanged rather than invented.
        assert_eq!(
            plain_path_text(r"\\?\Volume{0123}\repo"),
            r"\\?\Volume{0123}\repo"
        );
        assert_eq!(plain_path_text(r"C:\already\plain"), r"C:\already\plain");
    }

    /// Off Windows a backslash is a filename character and `\\?\` means
    /// nothing, so every spelling is left exactly as given.
    #[cfg(not(windows))]
    #[test]
    fn plain_path_text_is_the_identity_off_windows() {
        for path in [r"\\?\C:\repo\wt", "/tmp/repo/wt", r"/tmp/odd\name", ""] {
            assert_eq!(plain_path_text(path), path);
        }
    }

    /// Off Windows there is nothing to strip, and a backslash is an ordinary
    /// filename character -- rewriting one would rename the file.
    #[cfg(not(windows))]
    #[test]
    fn canonicalize_plain_is_canonicalize_off_windows() {
        let dir = tempfile::TempDir::new().unwrap();
        assert_eq!(
            canonicalize_plain(dir.path()).unwrap(),
            dir.path().canonicalize().unwrap()
        );
    }

    /// Script-host inputs are never resolved: naming a tool must not hand
    /// `wscript` a file this process did not mean to run.
    #[test]
    fn spawn_resolution_never_reaches_a_script_host_input() {
        let names = spawn_candidate_names("tool");
        for forbidden in [".vbs", ".js", ".wsf", ".jse", ".msc", ".ps1"] {
            assert!(
                !names.iter().any(|name| name.ends_with(forbidden)),
                "{forbidden} must not be a candidate: {names:?}"
            );
        }
    }

    /// Absolute paths pass through untouched, and a name found nowhere comes
    /// back unchanged so the spawn error keeps naming the requested tool.
    #[test]
    fn spawn_resolution_passes_through_separators_and_total_misses() {
        assert_eq!(
            resolve_spawn_program_with("/bin/sh", Some(std::ffi::OsStr::new("")), None),
            "/bin/sh"
        );
        assert_eq!(
            resolve_spawn_program_with(
                "gitpulse-no-such-tool",
                Some(std::ffi::OsStr::new("")),
                None
            ),
            "gitpulse-no-such-tool"
        );
    }

    /// The failure contract is preserved: an unresolvable program still fails
    /// `capture_command` with the original tool name in the message.
    #[test]
    fn capture_command_error_still_names_unresolvable_tool() {
        let err = capture_command(
            "gitpulse-no-such-tool-xyz",
            &[],
            None,
            Duration::from_secs(5),
            &[],
        )
        .expect_err("unresolvable tool must error");
        assert!(
            err.contains("gitpulse-no-such-tool-xyz"),
            "error must keep the bare tool name, got: {err}",
            err = err.message()
        );
    }

    #[test]
    fn git_command_strips_repo_redirect_env() {
        use std::collections::HashSet;
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let removed: HashSet<String> = cmd
            .get_envs()
            .filter(|(_, v)| v.is_none())
            .map(|(k, _)| k.to_string_lossy().into_owned())
            .collect();
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_ALTERNATE_OBJECT_DIRECTORIES",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_EXTERNAL_DIFF",
            "GIT_EXEC_PATH",
        ] {
            assert!(removed.contains(key), "git_command must remove {key}");
        }
    }

    /// The environment of every git child, from either builder. `gh` and the
    /// other captured tools shell out to git, so they must see the same locale,
    /// credential, lock and redirect settings git itself is given — the capture
    /// builder used to set only a third of them.
    fn git_environment_of(cmd: &Command) -> std::collections::HashMap<String, Option<String>> {
        cmd.get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect()
    }

    #[test]
    fn both_builders_pin_the_same_git_environment() {
        let git = git_environment_of(&git_command(Some(Path::new("/tmp")), &["status"]));
        let capture = git_environment_of(&build_capture_command_in(
            "gh",
            &["--version"],
            None,
            &[],
            None,
            None,
            PathSource::Inherited,
        ));
        for key in [
            "LC_ALL",
            "GCM_INTERACTIVE",
            "GIT_OPTIONAL_LOCKS",
            "GIT_TERMINAL_PROMPT",
            "GIT_EDITOR",
            "GIT_SEQUENCE_EDITOR",
        ] {
            assert!(
                git.get(key).is_some_and(Option::is_some),
                "git_command sets {key}"
            );
            assert_eq!(
                capture.get(key),
                git.get(key),
                "the capture builder's {key}"
            );
        }
        assert_eq!(git["LC_ALL"].as_deref(), Some("C"));
        for key in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_COMMON_DIR",
            "GIT_CONFIG_PARAMETERS",
            "GIT_SHALLOW_FILE",
            "GIT_ATTR_SOURCE",
            "GIT_LITERAL_PATHSPECS",
            "GIT_GLOB_PATHSPECS",
            "GIT_NOGLOB_PATHSPECS",
            "GIT_ICASE_PATHSPECS",
            "GIT_SSH",
            "GIT_SSH_COMMAND",
            "GIT_PROXY_COMMAND",
            "GIT_DIFF_OPTS",
        ] {
            assert_eq!(git.get(key), Some(&None), "git_command must remove {key}");
            assert_eq!(
                capture.get(key),
                Some(&None),
                "the capture builder must remove {key}"
            );
        }
    }

    /// Tracing floods stderr, of which only a few bytes are kept: the fatal
    /// line is the one that goes missing. Every `GIT_TRACE*` switch is removed
    /// by name or by prefix, including ones this build has never heard of.
    #[test]
    fn every_git_trace_switch_is_stripped() {
        let name = "GIT_TRACE2_EVENT_NESTING_FROM_THE_FUTURE";
        assert!(is_injected_git_env(name));
        for name in [
            "GIT_TRACE",
            "GIT_TRACE_PACKET",
            "GIT_TRACE2",
            "git_trace_setup",
        ] {
            assert!(is_injected_git_env(name), "{name}");
        }
        assert!(
            !is_injected_git_env("GIT_AUTHOR_NAME"),
            "identity is not redirection"
        );
    }

    /// A developer's `log.showSignature`, colour and diff-prefix settings change
    /// what the parsers read, so each rides every invocation; and a patch
    /// command never hands the diff to `diff.external`.
    #[test]
    fn git_command_pins_output_format_config_and_skips_the_external_diff() {
        let argv = |args: &[&str]| -> Vec<String> {
            git_command(Some(Path::new("/tmp")), args)
                .get_args()
                .map(|a| a.to_string_lossy().into_owned())
                .collect()
        };
        let log = argv(&["log", "-n1", "--format=%H"]);
        for pin in [
            "log.showSignature=false",
            "color.ui=never",
            "diff.noprefix=false",
            "diff.mnemonicPrefix=false",
        ] {
            assert!(
                log.windows(2).any(|pair| pair[0] == "-c" && pair[1] == pin),
                "missing -c {pin} in {log:?}"
            );
        }
        for patch in ["diff", "show", "log"] {
            let args = argv(&[patch, "HEAD"]);
            let at = args.iter().position(|a| a == patch).expect("subcommand");
            assert_eq!(args[at + 1], "--no-ext-diff", "{patch}: {args:?}");
            // `diff` rejects `--no-show-signature`; `show` and `log` carry it
            // so a signature header is never printed (and verified) for them.
            if patch == "diff" {
                assert_eq!(args[at + 2], "HEAD", "{patch}: {args:?}");
            } else {
                assert_eq!(args[at + 2], "--no-show-signature", "{patch}: {args:?}");
                assert_eq!(args[at + 3], "HEAD", "{patch}: {args:?}");
            }
        }
        let notes_log = argv(&["-c", "x.y=z", "log", "-n1"]);
        let at = notes_log.iter().position(|a| a == "log").unwrap();
        assert_eq!(notes_log[at + 2], "--no-show-signature", "{notes_log:?}");
        assert!(
            !argv(&["diff", "--cached"])
                .iter()
                .any(|a| a == "--no-show-signature"),
            "diff must not receive a flag it rejects"
        );
        // A global option with a value before the subcommand does not hide it,
        // and a command that prints no patch is left alone.
        let notes = argv(&["-c", "x.y=z", "notes", "show", "HEAD"]);
        assert!(!notes.iter().any(|a| a == "--no-ext-diff"), "{notes:?}");
        let global = argv(&["-C", "/tmp", "diff", "--cached"]);
        let at = global.iter().position(|a| a == "diff").unwrap();
        assert_eq!(global[at + 1], "--no-ext-diff", "{global:?}");
    }

    /// A batch stream that ends because its budget or deadline did keeps the
    /// answers that are whole and says, for each one it lost, that the output
    /// was cut: not "answered nothing", which would blame git for the cap.
    #[test]
    fn a_cut_batch_keeps_its_whole_answers_and_names_the_cut_for_the_rest() {
        const OBJECT: BatchObject = BatchObject {
            kind: "commit",
            noun: "commit",
        };
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let sent = [a.as_str(), b.as_str(), c.as_str()];
        let run = |stream: &[u8]| {
            let mut answers: HashMap<String, Result<usize, CommandError>> = HashMap::new();
            read_batch_answers(
                stream,
                &sent,
                OBJECT,
                &<[u8]>::len,
                Some("git cat-file output exceeded 64 bytes"),
                &mut answers,
            );
            answers
        };
        // The stream stops between answers: `c` was never started.
        let between = run(format!("{a} commit 2\nhi\n{b} commit 3\nabc\n").as_bytes());
        assert_eq!(between[&a], Ok(2));
        assert_eq!(between[&b], Ok(3));
        assert!(between[&c].as_ref().unwrap_err().contains("exceeded 64"));
        // The stream stops inside `b`'s body: `a` stays, `b` and `c` are cut.
        let inside = run(format!("{a} commit 2\nhi\n{b} commit 30\nabc").as_bytes());
        assert_eq!(inside[&a], Ok(2));
        for oid in [&b, &c] {
            let err = inside[oid].as_ref().unwrap_err();
            assert!(
                err.contains("exceeded 64") && err.contains(&b),
                "{err}",
                err = err.message()
            );
        }
    }

    /// An observer that keeps what it needs sees every byte of a long stream
    /// while the run retains almost none of it, and the byte cap still ends a
    /// stream that outruns it.
    #[cfg(unix)]
    #[test]
    fn a_draining_observer_streams_without_the_run_buffering_the_output() {
        struct Counting {
            seen: usize,
        }
        impl ProcessObserver for Counting {
            fn output(&mut self, stream: OutputStream, bytes: &[u8]) {
                if matches!(stream, OutputStream::Stdout) {
                    self.seen += bytes.len();
                }
            }
            fn drains_stdout(&self) -> bool {
                true
            }
        }
        let run_with_cap = |cap: usize| {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "head -c 3000000 /dev/zero"]);
            let mut observer = Counting { seen: 0 };
            let run = run_observed(
                &mut cmd,
                "sh",
                Duration::from_secs(30),
                None,
                cap,
                &mut observer,
            )
            .expect("run");
            (run, observer.seen)
        };
        let (whole, seen) = run_with_cap(4_000_000);
        assert!(whole.incomplete.is_none(), "{:?}", whole.incomplete);
        assert_eq!(seen, 3_000_000, "the observer saw the whole stream");
        assert!(
            whole.stdout.len() < 1_000_000,
            "the run kept {} of 3,000,000 bytes",
            whole.stdout.len()
        );
        let (cut, seen) = run_with_cap(100_000);
        assert_eq!(cut.incomplete, Some(Incomplete::OverCap(100_000)));
        assert!(
            seen <= 100_000,
            "the cap bounds a streamed read too: {seen}"
        );
    }

    /// The editor variables are PINNED rather than removed, and the difference
    /// is a security property, not a style choice.
    ///
    /// Removing `GIT_SEQUENCE_EDITOR` only clears the environment channel; git
    /// then falls back to the `sequence.editor` / `core.editor` config of
    /// whatever repository is open, and a repository directory carrying
    /// `sequence.editor = sh -c '…'` in its `.git/config` gets that command
    /// executed by any invocation that wants an editor. Pinning to `true`
    /// closes the config fallback as well as the environment one, and makes
    /// every editor-opening subcommand terminate immediately instead of
    /// blocking on a null stdin until the command timeout.
    #[test]
    fn git_command_pins_editors_to_a_harmless_program() {
        let cmd = git_command(Some(Path::new("/tmp")), &["rebase", "--continue"]);
        let pinned: std::collections::HashMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        for key in ["GIT_EDITOR", "GIT_SEQUENCE_EDITOR"] {
            assert_eq!(
                pinned.get(key).cloned().flatten().as_deref(),
                Some("true"),
                "{key} must be pinned to `true`, not merely removed"
            );
        }
    }

    /// Regression: without `-c core.quotepath=false`, `diff --numstat` quotes
    /// non-ASCII paths ("\\346...") while porcelain `-z` emits raw bytes, so
    /// the same file matches under two spellings. The config must ride every
    /// invocation assembled by `git_command`.
    #[test]
    fn git_command_disables_path_quoting() {
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let args: Vec<String> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert!(
            args.len() >= 2 && args[0] == "-c" && args[1] == "core.quotepath=false",
            "expected leading -c core.quotepath=false args, got {args:?}"
        );
    }

    #[test]
    fn network_timeout_exceeds_default_and_is_30_minutes() {
        assert_eq!(NETWORK_TIMEOUT, Duration::from_secs(30 * 60));
        assert!(NETWORK_TIMEOUT > DEFAULT_TIMEOUT);
    }

    /// A call that outlives its deadline must fail with the bounded runner's
    /// timeout wording (the same engine `git_with_timeout` drives), not hang.
    #[test]
    fn short_timeout_kills_slow_command_with_timeout_error() {
        let started = Instant::now();
        let outcome = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 30"]);
                cmd
            },
            "sh sleep 30",
            Duration::from_millis(300),
            None,
        );
        let Err(err) = outcome else {
            panic!("slow command must hit the deadline");
        };
        assert!(err.contains("timed out"), "got: {err}", err = err.message());
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
            "kill must be prompt, took {:?}",
            started.elapsed()
        );
    }

    #[test]
    fn capture_command_surfaces_timeout_wording() {
        let err = capture_command(
            "sh",
            &["-c", "sleep 30"],
            None,
            Duration::from_millis(250),
            &[],
        )
        .expect_err("must time out");
        assert!(
            err.contains("timed out after"),
            "got: {err}",
            err = err.message()
        );
    }

    #[test]
    fn a_complete_reader_still_reports_a_deadline_prefix_as_the_timeout() {
        let before = process_failures();
        let err = incomplete_is_failure(
            "log",
            Incomplete::Deadline {
                message: "git log timed out after 90s".into(),
                over_cap: None,
            },
        );
        assert_eq!(err, "git log timed out after 90s");
        assert!(!err.contains(SLOT_WAIT_SUFFIX), "{err}");
        assert!(!is_deferred_under_load(&err), "{err}");
        assert_eq!(process_failures(), before + 1);
        assert_eq!(
            last_process_failure()
                .map(CommandError::into_message)
                .as_deref(),
            Some("git log timed out after 90s")
        );
        let capped = incomplete_is_failure(
            "log",
            Incomplete::Deadline {
                message: "git log timed out after 90s".into(),
                over_cap: Some(1024),
            },
        );
        assert_eq!(capped, "git log output exceeded 1024 bytes");
        assert_eq!(
            process_failures(),
            before + 1,
            "a prefix that filled its byte budget is an answer with a reason"
        );
    }

    /// The pulse log is a capped read. Killing it at the deadline used to
    /// drop every commit git had already printed and surface
    /// `git log timed out after 90s` as a diagnostics error. The bytes stay,
    /// and a caller that did not opt into a prefix still gets the error.
    #[cfg(unix)]
    #[test]
    fn a_deadline_keeps_bytes_already_written_for_a_capped_read() {
        // `/bin/echo` exits before the sleep, so the line is in the pipe
        // even though a pipe is fully buffered for a still-running writer.
        let script = "/bin/echo pulse-prefix; sleep 30";
        let started = Instant::now();
        let kept = KeepDeadlinePrefix::enter();
        let run = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", script]);
                cmd
            },
            "git log",
            Duration::from_millis(300),
            None,
        )
        .expect("bytes written before the deadline are the result");
        drop(kept);
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
            "kill must be prompt, took {:?}",
            started.elapsed()
        );
        assert!(!run.success);
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("pulse-prefix"),
            "stdout: {}",
            String::from_utf8_lossy(&run.stdout)
        );
        match &run.incomplete {
            Some(Incomplete::Deadline {
                message,
                over_cap: None,
            }) => {
                assert!(message.contains("timed out after"), "{message}");
                assert!(!message.contains(SLOT_WAIT_SUFFIX), "{message}");
                assert!(!is_deferred_under_load(message), "{message}");
            }
            other => panic!("expected a deadline prefix, got {other:?}"),
        }

        let discarded = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", script]);
                cmd
            },
            "git log",
            Duration::from_millis(200),
            None,
        )
        .expect_err("a caller that needs the whole stream still fails");
        assert!(
            discarded.contains("timed out after"),
            "{discarded}",
            discarded = discarded.message()
        );
        assert!(
            !discarded.contains("pulse-prefix"),
            "{discarded}",
            discarded = discarded.message()
        );
    }

    /// A non-zero exit that printed something is the child failing, not a
    /// deadline. Keeping prefixes must not relabel that run.
    #[cfg(unix)]
    #[test]
    fn a_failing_child_is_not_a_deadline_prefix() {
        let _kept = KeepDeadlinePrefix::enter();
        let run = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "/bin/echo not-a-timeout; exit 1"]);
                cmd
            },
            "git log",
            Duration::from_secs(5),
            None,
        )
        .expect("exit 1 still finishes; it is not a deadline");
        assert!(!run.success);
        assert!(
            run.incomplete.is_none(),
            "exit 1 must not be a deadline prefix: {:?}",
            run.incomplete
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("not-a-timeout"),
            "stdout: {}",
            String::from_utf8_lossy(&run.stdout)
        );
    }

    /// No bytes and a deadline is not an empty successful read.
    #[cfg(unix)]
    #[test]
    fn an_empty_deadline_is_not_an_empty_success() {
        let _kept = KeepDeadlinePrefix::enter();
        let run = run_bounded(
            {
                let mut cmd = Command::new("sh");
                cmd.args(["-c", "sleep 30"]);
                cmd
            },
            "git log",
            Duration::from_millis(200),
            None,
        )
        .expect("the deadline is a prefix, including an empty one");
        assert!(run.stdout.is_empty());
        assert!(!run.success);
        assert!(matches!(
            run.incomplete,
            Some(Incomplete::Deadline { over_cap: None, .. })
        ));
    }

    /// Several capped reads killed together must each keep their own prefix
    /// and must not leave the sleep behind.
    #[cfg(unix)]
    #[test]
    fn concurrent_deadline_prefixes_are_reaped() {
        // A gate of its own with a slot per reader. The process-wide gate
        // sizes itself from the machine (`spawn_limit`): a 3-core runner gets
        // 6 slots, so two readers spent their whole 400ms queued and failed
        // as slot waits, never reaching the deadline prefix under test.
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(8)));
        let dir = tempfile::tempdir().unwrap();
        let mut handles = Vec::new();
        for index in 0..8 {
            let pidfile = dir.path().join(format!("pid-{index}"));
            handles.push(std::thread::spawn(move || {
                let script = format!(
                    "echo $$ > '{}'; /bin/echo row-{index}; sleep 30",
                    pidfile.display()
                );
                let _kept = KeepDeadlinePrefix::enter();
                let started = Instant::now();
                let mut cmd = Command::new("sh");
                cmd.args(["-c", &script]);
                let run = run_with_gate(
                    &mut cmd,
                    "git log",
                    Duration::from_millis(400),
                    None,
                    MAX_OUTPUT_BYTES,
                    &mut (),
                    gate,
                )
                .expect("prefix");
                assert!(
                    started.elapsed()
                        < crate::test_support::coverage_relaxed(Duration::from_secs(5)),
                    "reader {index} took {:?}",
                    started.elapsed()
                );
                assert!(String::from_utf8_lossy(&run.stdout).contains(&format!("row-{index}")));
                assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
                pidfile
            }));
        }
        let pidfiles: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("reader"))
            .collect();
        for pidfile in pidfiles {
            let pid = std::fs::read_to_string(&pidfile).expect("pid");
            let pid = pid.trim();
            let alive = crate::test_support::process_alive(pid.parse().expect("pid"));
            assert!(!alive, "deadline left pid {pid} running ({pidfile:?})");
        }
    }

    /// Regression (audit A4): the numbered-config channel injects arbitrary
    /// git config without any of the fixed names; SSH/askpass overrides can
    /// hijack transport and credentials. All must be classified as injected.
    #[test]
    fn numbered_config_and_transport_overrides_are_injected_env() {
        for name in [
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_KEY_0",
            "GIT_CONFIG_VALUE_0",
            "GIT_CONFIG_KEY_17",
            "git_config_key_3",
            "GIT_CONFIG_PARAMETERS",
            "GIT_CONFIG_GLOBAL",
            "GIT_CONFIG_SYSTEM",
            "GIT_SSH_COMMAND",
            "GIT_SSH_VARIANT",
            "GIT_ASKPASS",
            "GIT_CREDHELPER",
            "GIT_EXTERNAL_DIFF",
            "GIT_SEQUENCE_EDITOR",
            "GIT_EXEC_PATH",
            // Tracing floods the few bytes of stderr that are kept, and the
            // fatal line is the one that goes missing.
            "GIT_TRACE",
            "GIT_SSH",
            "GIT_PROXY_COMMAND",
            "GIT_DIFF_OPTS",
            "GIT_LITERAL_PATHSPECS",
        ] {
            assert!(is_injected_git_env(name), "{name} must be stripped");
        }
        for safe in ["GIT_AUTHOR_NAME", "GIT_PAGER", "EDITOR", "PATH"] {
            assert!(!is_injected_git_env(safe), "{safe} must survive");
        }
    }

    /// Regression (M4): GIT_CONFIG_PARAMETERS must be marked stripped on the
    /// spawned command. `env_remove` records the removal whether or not the
    /// parent environment carries the variable, so this needs no process-global
    /// mutation — which would race every other test that reads or spawns with
    /// the real environment in parallel.
    #[test]
    fn planted_git_config_parameters_is_removed_from_spawn_env() {
        let key = "GIT_CONFIG_PARAMETERS";
        let cmd = git_command(Some(Path::new("/tmp")), &["status"]);
        let removed = cmd
            .get_envs()
            .any(|(k, v)| k == std::ffi::OsStr::new(key) && v.is_none());
        assert!(
            removed,
            "planted GIT_CONFIG_PARAMETERS must be stripped from the spawned env"
        );
    }

    /// Captured external tools (`gh pr checkout` shells to git; npm/go/cargo
    /// read git config) inherit no injected-GIT-* redirection either, while an
    /// explicitly passed `extra_env` value still wins over the strip.
    #[test]
    fn capture_children_strip_injected_git_env_but_extra_env_wins() {
        let explicit_git_dir = "/explicit/caller-chosen";
        let cmd = build_capture_command(
            "gh",
            &["--version"],
            None,
            &[("GIT_DIR", explicit_git_dir)],
            None,
            None,
        );
        let value_of = |key: &str| -> Option<Option<std::ffi::OsString>> {
            cmd.get_envs()
                .find(|(k, _)| *k == std::ffi::OsStr::new(key))
                .map(|(_, v)| v.map(|v| v.to_os_string()))
        };
        for key in [
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_OBJECT_DIRECTORY",
            "GIT_COMMON_DIR",
            "GIT_NAMESPACE",
            "GIT_CONFIG",
            "GIT_CONFIG_COUNT",
            "GIT_CONFIG_PARAMETERS",
            "GIT_EXTERNAL_DIFF",
            "GIT_EXEC_PATH",
        ] {
            assert_eq!(
                value_of(key),
                Some(None),
                "{key} must be stripped from captured-tool children"
            );
        }
        // The editors are pinned, not stripped: captured tools shell out to
        // git, and removal alone leaves git's `core.editor` config fallback
        // open (a repository carrying `sequence.editor = sh -c '…'` would get
        // it executed). See git_command_pins_editors_to_a_harmless_program.
        for key in ["GIT_EDITOR", "GIT_SEQUENCE_EDITOR"] {
            assert_eq!(
                value_of(key)
                    .and_then(|v| v.map(|s| s.to_string_lossy().into_owned()))
                    .as_deref(),
                Some("true"),
                "{key} must be pinned for captured-tool children, not merely removed"
            );
        }
        assert_eq!(
            value_of("GIT_DIR").and_then(|v| v.map(|s| s.to_string_lossy().into_owned())),
            Some(explicit_git_dir.to_string()),
            "caller-supplied extra_env must override the strip"
        );
        // gh-specific prompt suppression travels with every captured child.
        assert_eq!(
            value_of("GH_PROMPT_DISABLED").map(|v| v.is_some()),
            Some(true)
        );
    }

    /// Regression (M3): a child that exits successfully while a daemonized
    /// grandchild keeps the stdout/stderr write ends open used to block the
    /// unconditional drain joins forever. The run must return promptly with
    /// the child's exit status.
    #[cfg(unix)]
    #[test]
    fn run_bounded_success_does_not_hang_on_grandchild_holding_pipes() {
        let started = Instant::now();
        let mut cmd = Command::new("sh");
        // `sh` backgrounds `sleep 30` (which inherits both pipe write ends)
        // and then itself exits 0 immediately.
        cmd.args(["-c", "sleep 30 & exit 0"]);
        let out = run_bounded(cmd, "sh", Duration::from_secs(5), None).expect("run");
        assert!(out.success);
        // The measured span is one `DRAIN_JOIN_GRACE` per pipe (the success
        // path gives stdout and stderr a window each) plus `spawn_gate()`
        // queueing, which parks on a condvar with no timeout and is therefore
        // unbounded whenever the parallel test harness has the gate saturated.
        // On a loaded macOS runner that queueing alone pushed this past an 8s
        // budget. What the assertion defends is that the call does not wait
        // out the 30s grandchild, so the budget stays far below it — the same
        // tolerance the sibling timeout test already carries.
        let budget = DRAIN_JOIN_GRACE * 2 + Duration::from_secs(10);
        assert!(
            started.elapsed() < budget,
            "drain collection must be grace-bounded (budget {budget:?}), took {:?}",
            started.elapsed()
        );
        // The status is trustworthy; the output is not. EOF never came, so
        // what was collected is a prefix of unknown length and must not be
        // handed on as the child's complete output.
        assert!(
            out.incomplete.is_some(),
            "an undelivered drain must report as a prefix, not as the whole stream"
        );
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("stdout incomplete"),
            "and it must say why: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        // And it must be the RIGHT prefix reason. `sh` printed nothing at all,
        // so anything that classifies this as an over-cap read is inventing a
        // cause — which is exactly how a 1,482-byte `for-each-ref` reached a
        // user's diagnostics log as "output exceeded 64 MB".
        assert!(
            matches!(out.incomplete, Some(Incomplete::Unread(_))),
            "an unread stream is not an over-cap one: {:?}",
            out.incomplete
        );
    }

    /// The end-to-end shape of the same defect: a caller that turns a prefix
    /// into an error must name what happened. `capture_command` is the seam
    /// every non-git tool goes through, and it shares `git_timeout`'s bug.
    #[cfg(unix)]
    #[test]
    fn an_unread_stream_is_not_reported_as_an_over_cap_one() {
        // Exits 0 immediately; the backgrounded child inherits the pipe write
        // ends, so EOF never arrives and the drain misses its window.
        let err = capture_command(
            "sh",
            &["-c", "sleep 30 & exit 0"],
            None,
            Duration::from_secs(5),
            &[],
        )
        .expect_err("a stdout of unknown completeness must not pass as complete");
        assert!(
            !err.contains("exceeded"),
            "nothing was printed, so nothing exceeded any cap: {err}",
            err = err.message()
        );
        assert!(
            err.contains("could not be read to the end"),
            "the error must name the real cause: {err}",
            err = err.message()
        );
    }

    /// The other arm still reads the way it always did, so fixing the wrong
    /// message did not cost the right one.
    #[test]
    fn an_over_cap_stream_still_reports_its_budget() {
        assert_eq!(
            Incomplete::OverCap(MAX_OUTPUT_BYTES).describe(),
            "exceeded 64 MB"
        );
        // A sub-megabyte budget must not round to "exceeded 0 MB".
        assert_eq!(Incomplete::OverCap(8_192).describe(), "exceeded 8192 bytes");
        assert!(Incomplete::Unread("reader did not finish".into())
            .describe()
            .contains("could not be read to the end"));
        // The clause carries no subject of its own, so a caller that names one
        // cannot end up saying "output output exceeded 64 MB".
        assert!(
            !Incomplete::OverCap(MAX_OUTPUT_BYTES)
                .describe()
                .contains("output"),
            "describe() must stay subject-free; callers supply the subject"
        );
    }

    /// Regression (M3): a timed-out child whose backgrounded grandchild keeps
    /// the pipes open must still yield the timeout error within the grace
    /// window instead of leaking unbounded blocking work into the caller.
    #[cfg(unix)]
    #[test]
    fn run_bounded_timeout_returns_despite_grandchild_holding_pipes() {
        let started = Instant::now();
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "sleep 30 & sleep 30"]);
        let err = run_bounded(cmd, "sh", Duration::from_secs(1), None).expect_err("timeout");
        assert!(err.contains("timed out"), "got: {err}", err = err.message());
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(15)),
            "timeout handling must stay bounded (one shared grace window), took {:?}",
            started.elapsed()
        );
    }

    /// A pipe that stops mid-stream must not read as a pipe that ended.
    ///
    /// The prefix is well-formed either way, so nothing downstream can tell
    /// the difference on the bytes alone: a half-read `git show -s --format=…`
    /// is a valid string with too few fields, which is how the parser came to
    /// report "Failed to parse commit metadata format" about a commit that was
    /// perfectly fine. Only this flag separates the two.
    #[test]
    fn a_broken_read_is_not_reported_as_the_end_of_the_stream() {
        struct BreaksAfter(&'static [u8], bool);
        impl Read for BreaksAfter {
            fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
                if self.1 {
                    return Err(std::io::Error::other("pipe went away"));
                }
                self.1 = true;
                let n = self.0.len().min(buf.len());
                buf[..n].copy_from_slice(&self.0[..n]);
                Ok(n)
            }
        }

        let broken = drain_capped(Some(BreaksAfter(b"abc\0def", false)), 1024);
        assert_eq!(broken.bytes, b"abc\0def");
        assert!(!broken.truncated, "it did not hit the byte cap");
        assert!(
            matches!(broken.stop, Some(Stop::Broken(_))),
            "a read that failed must say so, or its prefix passes for the whole output"
        );

        let clean = drain_capped(Some(std::io::Cursor::new(b"abc\0def".to_vec())), 1024);
        assert_eq!(clean.bytes, b"abc\0def");
        assert!(clean.stop.is_none(), "a clean read reports no stop reason");

        let capped = drain_capped(Some(std::io::Cursor::new(vec![b'x'; 64])), 8);
        assert_eq!(capped.bytes.len(), 8);
        assert!(capped.truncated, "the cap is truncation, not failure");
        assert!(capped.stop.is_none());

        // No pipe at all is an empty stream, not a broken one.
        let none = drain_capped(None::<std::io::Cursor<Vec<u8>>>, 8);
        assert!(none.bytes.is_empty() && !none.truncated && none.stop.is_none());
    }

    /// A drain that never delivered produced *unknown*, and "the child printed
    /// nothing" is a real, common answer — so the two must not arrive as the
    /// same empty value.
    #[test]
    fn an_undelivered_drain_is_not_an_empty_output() {
        let (tx, rx) = mpsc::channel::<Drained>();
        let deadline = Instant::now() + Duration::from_millis(20);
        let missing = collect_drained_deadline(&rx, deadline);
        assert!(
            matches!(missing.stop, Some(Stop::Undelivered(_))),
            "nothing arrived, which is not the same as nothing being there"
        );
        assert!(missing.bytes.is_empty());

        tx.send(Drained::default()).expect("send");
        let delivered = collect_drained_deadline(&rx, Instant::now() + Duration::from_secs(1));
        assert!(
            delivered.stop.is_none(),
            "a child that genuinely printed nothing reports no stop reason"
        );
        assert!(delivered.bytes.is_empty());
    }

    /// `git_captured` exists so a non-zero exit can reach the caller as data.
    /// If it flattened that into `Err` like [`git`] does, the caller could not
    /// tell "git looked and found nothing" from "git could not look" — and a
    /// failure to spawn must never arrive as an ordinary empty answer.
    #[test]
    fn git_captured_reports_a_non_zero_exit_as_data_and_a_spawn_failure_as_an_error() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        crate::test_support::git_in(repo, &["init", "-b", "main"]);

        // Exit 1, empty stdout: an answer.
        let run = git_captured(repo, &["notes", "--ref=refs/notes/none", "show", "HEAD"])
            .expect("a non-zero exit is not a failure to run");
        assert!(!run.success, "git said no");
        assert_ne!(run.status_code, 0);
        assert!(run.incomplete.is_none());

        // A successful run still reports success and carries stdout.
        let ok = git_captured(repo, &["rev-parse", "--is-inside-work-tree"]).expect("ran");
        assert!(ok.success);
        assert_eq!(String::from_utf8_lossy(&ok.stdout).trim(), "true");

        // A path that is not a repository fails to run at all, and says so.
        let outside = tempfile::tempdir().expect("tempdir");
        let err = git_captured(outside.path(), &["notes", "list"])
            .map(|r| r.success)
            .unwrap_or(false);
        assert!(!err, "a non-repository must not report a successful run");
    }

    /// The gate's own contract, checked without racing a real process: once
    /// `limit` permits are out, the next acquire parks until one is dropped.
    #[test]
    fn spawn_gate_parks_callers_once_the_limit_is_reached() {
        let gate = std::sync::Arc::new(SpawnGate::new(2));
        let first = gate
            .acquire(Instant::now() + Duration::from_secs(30))
            .unwrap();
        let second = gate
            .acquire(Instant::now() + Duration::from_secs(30))
            .unwrap();
        assert_eq!(gate.peak(), 2);

        let (tx, rx) = mpsc::channel();
        let waiter = {
            let gate = std::sync::Arc::clone(&gate);
            thread::spawn(move || {
                let permit = gate
                    .acquire(Instant::now() + Duration::from_secs(30))
                    .unwrap();
                let _ = tx.send(());
                drop(permit);
            })
        };

        assert!(
            rx.recv_timeout(Duration::from_millis(250)).is_err(),
            "a third caller must park while both permits are held"
        );
        drop(second);
        rx.recv_timeout(Duration::from_secs(5))
            .expect("dropping a permit must wake a parked caller");
        waiter.join().expect("waiter thread");
        drop(first);
        assert_eq!(
            gate.peak(),
            2,
            "the high-water mark must never exceed the limit"
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_commands_expire_before_a_subprocess_slot_is_available() {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
        let permits: Vec<_> = (0..gate.limit)
            .map(|_| {
                gate.acquire(Instant::now() + Duration::from_secs(10))
                    .expect("test permit")
            })
            .collect();
        let (tx, rx) = mpsc::channel();
        let child_gate = gate;
        let worker = thread::spawn(move || {
            let result = run_with_gate(
                &mut Command::new("/usr/bin/true"),
                "queue-test",
                Duration::from_millis(20),
                None,
                MAX_OUTPUT_BYTES,
                &mut (),
                child_gate,
            );
            tx.send(result).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_millis(300));
        drop(permits);
        worker.join().unwrap();
        let error = result
            .expect("queue wait must obey the command deadline")
            .expect_err("the queued command must not start");
        assert!(
            error.contains("waiting for a process slot"),
            "{error}",
            error = error.message()
        );
        assert!(
            error.contains(TIMEOUT_MARKER),
            "{error}",
            error = error.message()
        );
    }

    /// Deterministic, dependency-free generator for the stress tests: a fixed
    /// seed reproduces the exact schedule of classes, deadlines and cancels.
    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            self.0 >> 33
        }
    }

    /// `cancelled` is caller code. A waiter whose check panicked left its
    /// waiting reservation behind, and a stranded interactive or background
    /// reservation makes every later refresh ineligible for good.
    #[test]
    fn a_panicking_cancel_check_does_not_strand_its_reservation() {
        use super::{with_admission, Admission};
        for class in [Admission::Interactive, Admission::Background] {
            let gate = Arc::new(SpawnGate::new(1));
            let held = gate
                .acquire(Instant::now() + Duration::from_secs(1))
                .unwrap();
            let waiter = {
                let gate = Arc::clone(&gate);
                thread::spawn(move || {
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        with_admission(class, || {
                            let _ = gate
                                .acquire_until(Instant::now() + Duration::from_secs(2), &|| {
                                    panic!("observer failed")
                                });
                        })
                    }))
                    .is_err()
                })
            };
            let (tx, rx) = mpsc::channel();
            thread::spawn(move || {
                let _ = tx.send(waiter.join());
            });
            let panicked = rx
                .recv_timeout(Duration::from_secs(5))
                .expect("the waiter never finished")
                .expect("the waiter thread itself panicked");
            assert!(panicked, "the check must have panicked inside the gate");
            drop(held);
            assert!(
                gate.acquire(Instant::now() + Duration::from_millis(300))
                    .is_some(),
                "{class:?} reservation stranded by a panicking check"
            );
            let state = gate
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            assert_eq!(
                (state.waiting_interactive, state.waiting_background),
                (0, 0)
            );
        }
    }

    /// A cancel check that took a lock of its own, or re-entered the gate,
    /// deadlocked against a gate that called it while holding its state.
    #[test]
    fn the_cancel_check_runs_without_the_gate_lock() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let gate = Arc::new(SpawnGate::new(1));
        let held = gate
            .acquire(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let checked = AtomicBool::new(false);
        let lock_free = AtomicBool::new(true);
        let probe = Arc::clone(&gate);
        let outcome = gate.acquire_until(Instant::now() + Duration::from_millis(150), &|| {
            checked.store(true, Ordering::Relaxed);
            if probe.state.try_lock().is_err() {
                lock_free.store(false, Ordering::Relaxed);
            }
            false
        });
        assert!(outcome.is_err(), "the only slot is held");
        assert!(checked.load(Ordering::Relaxed), "the check never ran");
        assert!(
            lock_free.load(Ordering::Relaxed),
            "the cancel check ran while the gate held its own lock"
        );
        drop(held);
    }

    /// Seeded stress across every class, deadline and cancellation shape. It
    /// asserts the gate's invariants, never a duration: a slow host only
    /// loosens the rate bound.
    #[test]
    fn mixed_class_stress_holds_every_gate_invariant() {
        use super::{with_admission, Admission};
        use std::sync::atomic::{AtomicU64, Ordering};
        const THREADS: u64 = 24;
        const ROUNDS: usize = 40;
        let (limit, burst, rate) = (8usize, 16u32, 20u32);
        let gate = Arc::new(SpawnGate::with_rate(limit, burst, rate));
        let attempts = Arc::new([AtomicU64::new(0), AtomicU64::new(0), AtomicU64::new(0)]);
        let violations = Arc::new(Mutex::new(Vec::<String>::new()));
        let started = Instant::now();
        let (done_tx, done_rx) = mpsc::channel();
        for seed in 0..THREADS {
            let gate = Arc::clone(&gate);
            let attempts = Arc::clone(&attempts);
            let violations = Arc::clone(&violations);
            let done_tx = done_tx.clone();
            thread::spawn(move || {
                let mut rng = Lcg(0x9E37_79B9_7F4A_7C15 ^ (seed + 1).wrapping_mul(0xBF58_476D));
                for _ in 0..ROUNDS {
                    let class = Admission::ALL[(rng.next() % 3) as usize];
                    let wait = Duration::from_millis(1 + rng.next() % 30);
                    let cancel_at = rng
                        .next()
                        .is_multiple_of(4)
                        .then(|| Instant::now() + Duration::from_millis(rng.next() % 10));
                    attempts[class.index()].fetch_add(1, Ordering::Relaxed);
                    let outcome = with_admission(class, || {
                        gate.acquire_until(Instant::now() + wait, &|| {
                            cancel_at.is_some_and(|at| Instant::now() >= at)
                        })
                    });
                    if let Ok(permit) = outcome {
                        {
                            let state = gate
                                .state
                                .lock()
                                .unwrap_or_else(std::sync::PoisonError::into_inner);
                            let mut found = violations.lock().unwrap();
                            if state.in_flight > limit {
                                found.push(format!("in_flight {} > {limit}", state.in_flight));
                            }
                            if state.background > (limit / 4).max(1) {
                                found.push(format!("background {}", state.background));
                            }
                        }
                        let hold = rng.next() % 3;
                        if hold > 0 {
                            thread::sleep(Duration::from_millis(hold));
                        }
                        drop(permit);
                    }
                }
                let _ = done_tx.send(());
            });
        }
        drop(done_tx);
        for _ in 0..THREADS {
            done_rx
                .recv_timeout(Duration::from_secs(60))
                .expect("a stress thread hung inside the gate");
        }
        let elapsed = started.elapsed();
        let found = violations.lock().unwrap();
        assert!(found.is_empty(), "{found:?}");
        let state = gate.state.lock().unwrap();
        assert_eq!(
            (
                state.in_flight,
                state.background,
                state.waiting_background,
                state.waiting_interactive
            ),
            (0, 0, 0, 0),
            "a reservation outlived its waiter"
        );
        assert!(state.peak <= limit);
        for class in Admission::ALL {
            let c = state.counters[class.index()];
            assert_eq!(
                c.admitted + c.refused + c.shed + c.timed_out + c.cancelled,
                attempts[class.index()].load(Ordering::Relaxed),
                "{class:?}: every attempt has exactly one outcome: {c:?}"
            );
        }
        let rate_limited = state.counters[Admission::Reactive.index()].admitted
            + state.counters[Admission::Background.index()].admitted;
        let bound = u64::from(burst) + (f64::from(rate) * elapsed.as_secs_f64()).ceil() as u64 + 1;
        assert!(
            rate_limited <= bound,
            "{rate_limited} rate-limited admissions in {elapsed:?}; the cap allows {bound}"
        );
        let reactive = state.counters[Admission::Reactive.index()];
        assert!(
            reactive.refused + reactive.timed_out + reactive.cancelled > 0,
            "the schedule never pushed the gate past its limits: {reactive:?}"
        );
        assert!(state.counters[Admission::Interactive.index()].admitted > 0);
    }

    /// The hazard shared reads exist to avoid, under load: a caller must
    /// never be answered by a child that started before it asked. A writer
    /// bumps an epoch file (by rename) and publishes the new value only
    /// afterwards; each child `cat`s the file, so its answer is at least the
    /// epoch its caller saw on arrival — unless it joined an older child.
    #[cfg(unix)]
    #[test]
    fn a_shared_read_never_answers_with_a_child_older_than_its_caller() {
        use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(2)));
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        std::fs::write(cwd.join("epoch"), "0").unwrap();
        let published = Arc::new(AtomicU64::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let writer = {
            let (cwd, published, stop) = (cwd.clone(), Arc::clone(&published), Arc::clone(&stop));
            thread::spawn(move || {
                let mut epoch = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    epoch += 1;
                    let tmp = cwd.join("epoch.tmp");
                    std::fs::write(&tmp, epoch.to_string()).unwrap();
                    std::fs::rename(&tmp, cwd.join("epoch")).unwrap();
                    published.store(epoch, Ordering::SeqCst);
                    thread::sleep(Duration::from_millis(2));
                }
            })
        };
        // Start with every slot held so the first wave provably shares.
        let held: Vec<_> = (0..2)
            .map(|_| {
                gate.acquire(Instant::now() + Duration::from_secs(1))
                    .unwrap()
            })
            .collect();
        let (done_tx, done_rx) = mpsc::channel();
        for reader in 0..8u64 {
            let (cwd, published, done_tx) = (cwd.clone(), Arc::clone(&published), done_tx.clone());
            thread::spawn(move || {
                let mut stale = Vec::new();
                // Without jitter the readers stay in lockstep — all answered
                // together, all asking again together — so every join lands
                // before the child starts and the hazard is never exercised.
                let mut rng = Lcg(0xD1B5_4A32_D192_ED03 ^ (reader + 1));
                for _ in 0..25 {
                    thread::sleep(Duration::from_millis(rng.next() % 26));
                    let arrived = published.load(Ordering::SeqCst);
                    // Read first, then linger: a caller that joins this child
                    // after it started is then provably answered with an
                    // epoch older than the one it arrived at.
                    let mut cmd = Command::new("/bin/sh");
                    cmd.args(["-c", "cat epoch; sleep 0.02"]).current_dir(&cwd);
                    let run =
                        super::run_read_shared(cmd, "epoch", Duration::from_secs(10), 64, gate)
                            .expect("epoch read");
                    let seen: u64 = String::from_utf8_lossy(&run.stdout).trim().parse().unwrap();
                    if seen < arrived {
                        stale.push((arrived, seen));
                    }
                }
                let _ = done_tx.send(stale);
            });
        }
        drop(done_tx);
        let deadline = Instant::now() + Duration::from_secs(5);
        while gate.counters()[super::Admission::Reactive.index()].coalesced == 0 {
            assert!(
                Instant::now() < deadline,
                "the first wave never shared a child"
            );
            thread::yield_now();
        }
        drop(held);
        let mut stale = Vec::new();
        for _ in 0..8 {
            stale.extend(
                done_rx
                    .recv_timeout(Duration::from_secs(60))
                    .expect("a reader hung"),
            );
        }
        stop.store(true, Ordering::Relaxed);
        writer.join().unwrap();
        assert!(
            stale.is_empty(),
            "answered from before the caller arrived: {stale:?}"
        );
        // Every call either started a child or joined one; nothing is lost
        // and nothing is counted twice.
        let children = spawn_log::spawns_in(&cwd).len() as u64;
        let joined = gate.counters()[super::Admission::Reactive.index()].coalesced;
        assert_eq!(
            children + joined,
            8 * 25,
            "{children} children + {joined} joins"
        );
        eprintln!("shared-read stress: {children} children served 200 reads ({joined} joined)");
        let leftover = super::shared_reads()
            .lock()
            .unwrap()
            .keys()
            .filter(|key| key.cwd == cwd)
            .count();
        assert_eq!(leftover, 0, "a finished read stayed joinable");
    }

    /// Identical reads queued behind a full gate share one child. Each child
    /// appends its pid to a file, so the count is of processes, not of
    /// results.
    #[cfg(unix)]
    #[test]
    fn identical_queued_reads_share_one_child() {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(1)));
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let pids = cwd.join("pids");
        let script = format!("echo $$ >> '{}'; echo shared", pids.display());
        let read = {
            let cwd = cwd.clone();
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", script.as_str()]).current_dir(&cwd);
                super::run_read_shared(cmd, "shared-read", Duration::from_secs(10), 4096, gate)
            }
        };
        let held = gate
            .acquire(Instant::now() + Duration::from_secs(1))
            .unwrap();
        let readers: Vec<_> = (0..3)
            .map(|_| {
                let read = read.clone();
                thread::spawn(read)
            })
            .collect();
        let deadline = Instant::now() + Duration::from_secs(5);
        while gate.counters()[super::Admission::Reactive.index()].coalesced < 2 {
            assert!(
                Instant::now() < deadline,
                "the readers never queued together"
            );
            thread::yield_now();
        }
        drop(held);
        for reader in readers {
            let run = reader.join().unwrap().expect("shared read");
            assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "shared");
        }
        let started = std::fs::read_to_string(&pids).unwrap();
        assert_eq!(
            started.lines().count(),
            1,
            "three queued reads, one child: {started}"
        );
    }

    /// A read that arrives once the leader's child is running must start its
    /// own: it may be asking because of a write that child cannot have seen.
    #[cfg(unix)]
    #[test]
    fn a_read_arriving_after_the_child_started_does_not_join_it() {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(4)));
        let dir = tempfile::TempDir::new().unwrap();
        let cwd = dir.path().canonicalize().unwrap();
        let pids = cwd.join("pids");
        let script = format!("echo $$ >> '{}'; sleep 0.4", pids.display());
        let read = {
            let cwd = cwd.clone();
            move || {
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", script.as_str()]).current_dir(&cwd);
                super::run_read_shared(cmd, "late-read", Duration::from_secs(10), 4096, gate)
            }
        };
        let first = thread::spawn(read.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while std::fs::read_to_string(&pids).map_or(0, |s| s.lines().count()) == 0 {
            assert!(Instant::now() < deadline, "the first child never started");
            thread::yield_now();
        }
        let second = thread::spawn(read);
        first.join().unwrap().expect("first read");
        second.join().unwrap().expect("second read");
        let started = std::fs::read_to_string(&pids).unwrap();
        assert_eq!(
            started.lines().count(),
            2,
            "the late read joined a running child: {started}"
        );
        assert_eq!(
            gate.counters()[super::Admission::Reactive.index()].coalesced,
            0
        );
    }

    /// The harness for joiner-deadline tests: a gate whose only slot is held,
    /// so every read queues and the first caller leads a flight the rest join.
    #[cfg(unix)]
    struct JoinerRig {
        gate: &'static SpawnGate,
        _dir: tempfile::TempDir,
        cwd: PathBuf,
        held: Vec<super::SpawnPermit<'static>>,
    }

    #[cfg(unix)]
    impl JoinerRig {
        fn new() -> Self {
            Self::with_slots(1)
        }

        /// A gate of `slots` slots, every one held until [`Self::release`].
        fn with_slots(slots: usize) -> Self {
            let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(slots)));
            let dir = tempfile::TempDir::new().unwrap();
            let cwd = dir.path().canonicalize().unwrap();
            let held = (0..slots)
                .map(|_| {
                    gate.acquire(Instant::now() + Duration::from_secs(5))
                        .unwrap()
                })
                .collect();
            Self {
                gate,
                _dir: dir,
                cwd,
                held,
            }
        }

        /// A read of `script` (run by `sh -c`) in the rig's directory.
        fn read(
            &self,
            script: &str,
            timeout: Duration,
        ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
            self.read_as(script, timeout, false)
        }

        /// [`Self::read`] from a caller that keeps a deadline's prefix, as
        /// `git_run_inner` does for every production read.
        fn read_keeping(
            &self,
            script: &str,
            timeout: Duration,
        ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
            self.read_as(script, timeout, true)
        }

        fn read_as(
            &self,
            script: &str,
            timeout: Duration,
            keep_prefix: bool,
        ) -> thread::JoinHandle<(Result<super::BoundedRun, CommandError>, Duration)> {
            let (gate, cwd, script) = (self.gate, self.cwd.clone(), script.to_string());
            thread::spawn(move || {
                let _kept = keep_prefix.then(KeepDeadlinePrefix::enter);
                let mut cmd = Command::new("/bin/sh");
                cmd.args(["-c", script.as_str()]).current_dir(&cwd);
                let started = Instant::now();
                let result = super::run_read_shared(cmd, "joined-read", timeout, 4096, gate);
                (result, started.elapsed())
            })
        }

        fn flight_open(&self) -> bool {
            super::shared_reads()
                .lock()
                .unwrap()
                .keys()
                .any(|key| key.cwd == self.cwd)
        }

        fn wait_for_flight(&self) {
            let until = Instant::now() + Duration::from_secs(5);
            while !self.flight_open() {
                assert!(Instant::now() < until, "the leader never queued");
                thread::yield_now();
            }
        }

        fn wait_for_joins(&self, joins: u64) {
            let until = Instant::now() + Duration::from_secs(5);
            while self.gate.counters()[super::Admission::Reactive.index()].coalesced < joins {
                assert!(Instant::now() < until, "the joiners never queued");
                thread::yield_now();
            }
        }

        fn release(&mut self) {
            self.held.clear();
        }
    }

    /// A joiner returns by its own deadline, not the leader's child's, and the
    /// leader neither dies nor loses its answer.
    #[cfg(unix)]
    #[test]
    fn a_joiner_returns_by_its_own_deadline_while_the_leader_runs_on() {
        let mut rig = JoinerRig::new();
        let leader = rig.read("sleep 2; echo leader-done", Duration::from_secs(10));
        rig.wait_for_flight();
        let joiner = rig.read("sleep 2; echo leader-done", Duration::from_millis(200));
        rig.wait_for_joins(1);
        rig.release();
        let (joined, waited) = joiner.join().unwrap();
        let error = joined.expect_err("the joiner's 200 ms ran out");
        assert_eq!(error, format!("joined-read{TIMEOUT_MARKER}0.2s"));
        assert!(is_timed_out(error.message()));
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(1200)),
            "the joiner sat for {waited:?}"
        );
        let (led, _) = leader.join().unwrap();
        let led = led.expect("the leader was not disturbed");
        assert!(led.success && led.incomplete.is_none(), "{led:?}");
        assert_eq!(String::from_utf8_lossy(&led.stdout).trim(), "leader-done");
    }

    /// The leader's deadline is the leader's: a joiner with budget left runs
    /// its own read instead of being handed the leader's timeout.
    #[cfg(unix)]
    #[test]
    fn a_joiner_with_budget_left_does_not_inherit_the_leaders_timeout() {
        let mut rig = JoinerRig::new();
        let script = "if [ -e ran ]; then echo second; else touch ran; sleep 5; fi";
        let leader = rig.read(script, Duration::from_millis(300));
        rig.wait_for_flight();
        let joiner = rig.read(script, Duration::from_secs(10));
        rig.wait_for_joins(1);
        rig.release();
        let (led, _) = leader.join().unwrap();
        let error = led.expect_err("the leader's 300 ms ran out");
        assert!(
            is_timed_out(error.message()),
            "{error}",
            error = error.message()
        );
        let (joined, _) = joiner.join().unwrap();
        let run = joined.expect("the joiner had budget to run its own read");
        assert!(run.success && run.incomplete.is_none(), "{run:?}");
        assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "second");
    }

    /// The leader's deadline error is a joiner's answer once the joiner's own
    /// budget is gone too: it does not loop on a clock that has already run.
    #[cfg(unix)]
    #[test]
    fn a_joiner_whose_budget_also_ran_out_reports_its_own_timeout() {
        let mut rig = JoinerRig::new();
        let leader = rig.read("sleep 5", Duration::from_millis(300));
        rig.wait_for_flight();
        let joiner = rig.read("sleep 5", Duration::from_millis(350));
        rig.wait_for_joins(1);
        rig.release();
        let (led, _) = leader.join().unwrap();
        assert!(is_timed_out(led.expect_err("leader deadline").message()));
        let (joined, waited) = joiner.join().unwrap();
        let error = joined.expect_err("joiner deadline");
        assert!(
            is_timed_out(error.message()),
            "{error}",
            error = error.message()
        );
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
            "{waited:?}"
        );
    }

    /// Many joiners with mixed deadlines each leave at their own, and the
    /// joiner that outlasts the child shares it: one process in all.
    #[cfg(unix)]
    #[test]
    fn joiners_with_mixed_deadlines_each_leave_at_their_own() {
        let mut rig = JoinerRig::new();
        let script = format!(
            "echo $$ >> '{}/pids'; sleep 1.5; echo shared",
            rig.cwd.display()
        );
        let leader = rig.read(&script, Duration::from_secs(10));
        rig.wait_for_flight();
        let short: Vec<_> = [150u64, 300, 450, 150, 300]
            .iter()
            .map(|ms| (*ms, rig.read(&script, Duration::from_millis(*ms))))
            .collect();
        let long = rig.read(&script, Duration::from_secs(10));
        rig.wait_for_joins(6);
        rig.release();
        for (ms, joiner) in short {
            let (joined, waited) = joiner.join().unwrap();
            let error = joined.expect_err("a short joiner outlasted by the child");
            assert!(
                is_timed_out(error.message()),
                "{error}",
                error = error.message()
            );
            assert!(
                waited
                    < Duration::from_millis(ms)
                        + crate::test_support::coverage_relaxed(Duration::from_millis(900)),
                "{ms} ms joiner took {waited:?}"
            );
        }
        for handle in [leader, long] {
            let (run, _) = handle.join().unwrap();
            let run = run.expect("the shared read finished");
            assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "shared");
        }
        let started = std::fs::read_to_string(rig.cwd.join("pids")).unwrap();
        assert_eq!(started.lines().count(), 1, "one child for all: {started}");
    }

    /// A joiner still hears a leader that panics: the wait is bounded, but the
    /// wake from `Publish` stays.
    #[cfg(unix)]
    #[test]
    fn a_leader_that_panics_still_wakes_a_joiner_before_its_deadline() {
        let rig = JoinerRig::new();
        let (gate, cwd) = (rig.gate, rig.cwd.clone());
        let leader = thread::spawn(move || {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "true"]).current_dir(&cwd);
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                super::with_forced_spawn_failure_then(
                    move || {
                        let until = Instant::now() + Duration::from_secs(5);
                        while gate.counters()[super::Admission::Reactive.index()].coalesced < 1
                            && Instant::now() < until
                        {
                            thread::yield_now();
                        }
                        panic!("the leader's read panicked");
                    },
                    || {
                        super::run_read_shared(
                            cmd,
                            "joined-read",
                            Duration::from_secs(10),
                            4096,
                            gate,
                        )
                    },
                )
            }));
        });
        rig.wait_for_flight();
        let joiner = rig.read("true", Duration::from_secs(10));
        let (joined, waited) = joiner.join().unwrap();
        leader.join().unwrap();
        let error = joined.expect_err("a panicked leader has no answer");
        assert!(
            error.contains("ended without a result"),
            "{error}",
            error = error.message()
        );
        assert!(waited < Duration::from_secs(4), "{waited:?}");
    }

    /// A joiner handed almost no time, as a lock retry passes, answers with
    /// its timeout at once instead of waiting on the leader.
    #[cfg(unix)]
    #[test]
    fn a_joiner_with_almost_no_time_left_does_not_wait_for_the_leader() {
        let mut rig = JoinerRig::new();
        let leader = rig.read("sleep 1.5; echo done", Duration::from_secs(10));
        rig.wait_for_flight();
        let joiner = rig.read("sleep 1.5; echo done", Duration::from_millis(1));
        rig.wait_for_joins(1);
        rig.release();
        let (joined, waited) = joiner.join().unwrap();
        assert!(is_timed_out(joined.expect_err("1 ms").message()));
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(800)),
            "{waited:?}"
        );
        assert!(leader.join().unwrap().0.expect("leader").success);
    }

    /// A joiner whose deadline falls exactly when the leader publishes gets
    /// one coherent answer, never a hang and never a torn result.
    #[cfg(unix)]
    #[test]
    fn a_joiner_deadline_racing_the_publish_gets_one_coherent_answer() {
        for round in 0..10u64 {
            let mut rig = JoinerRig::new();
            let leader = rig.read("sleep 0.3; echo raced", Duration::from_secs(10));
            rig.wait_for_flight();
            let joiner = rig.read(
                "sleep 0.3; echo raced",
                Duration::from_millis(300 + round * 3),
            );
            rig.wait_for_joins(1);
            rig.release();
            let (joined, waited) = joiner.join().unwrap();
            match joined {
                Ok(run) => assert_eq!(String::from_utf8_lossy(&run.stdout).trim(), "raced"),
                Err(error) => assert!(
                    is_timed_out(error.message()),
                    "{error}",
                    error = error.message()
                ),
            }
            assert!(waited < Duration::from_secs(3), "{waited:?}");
            assert!(leader.join().unwrap().0.expect("leader").success);
        }
    }

    /// A caller that keeps deadline prefixes, as every production read does,
    /// gets the marked form when its joiner deadline passes. The joiner has no
    /// bytes of its own: the child is the leader's.
    #[cfg(unix)]
    #[test]
    fn a_keeping_joiner_that_runs_out_reports_a_marked_empty_run() {
        let mut rig = JoinerRig::new();
        let leader = rig.read_keeping("sleep 2; echo late", Duration::from_secs(10));
        rig.wait_for_flight();
        let joiner = rig.read_keeping("sleep 2; echo late", Duration::from_millis(200));
        rig.wait_for_joins(1);
        rig.release();
        let (joined, _) = joiner.join().unwrap();
        let run = joined.expect("a keeping caller gets the prefix form, not an error");
        assert!(!run.success && run.stdout.is_empty(), "{run:?}");
        let Some(Incomplete::Deadline { message, over_cap }) = &run.incomplete else {
            panic!("expected a deadline marker: {run:?}");
        };
        assert_eq!(message, &format!("joined-read{TIMEOUT_MARKER}0.2s"));
        assert!(over_cap.is_none());
        assert!(leader.join().unwrap().0.expect("leader").success);
    }

    /// The leader's prefix is the best answer a keeping joiner can get when
    /// its own rerun has too little time left to print anything: a solo call
    /// with the joiner's budget would have returned those bytes.
    #[cfg(unix)]
    #[test]
    fn a_rerunning_joiner_keeps_the_leaders_prefix_when_its_own_run_yields_less() {
        let mut rig = JoinerRig::new();
        let script = "if [ -e ran ]; then sleep 5; else touch ran; echo partial; sleep 5; fi";
        let leader = rig.read_keeping(script, Duration::from_millis(400));
        rig.wait_for_flight();
        let joiner = rig.read_keeping(script, Duration::from_millis(900));
        rig.wait_for_joins(1);
        rig.release();
        let (led, _) = leader.join().unwrap();
        let led = led.expect("keeping leader");
        assert_eq!(String::from_utf8_lossy(&led.stdout).trim(), "partial");
        let (joined, waited) = joiner.join().unwrap();
        let run = joined.expect("keeping joiner");
        assert!(
            matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
            "{run:?}"
        );
        assert_eq!(
            String::from_utf8_lossy(&run.stdout).trim(),
            "partial",
            "the leader's prefix was thrown away"
        );
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(2500)),
            "{waited:?}"
        );
    }

    /// A rerun spends what is left of the caller's own budget, never a fresh
    /// one: a leader's timeout does not give its joiner more time in all, and
    /// the timeout it reports is still the budget it was given.
    #[cfg(unix)]
    #[test]
    fn a_rerun_never_outlasts_the_joiners_total_budget() {
        let mut rig = JoinerRig::new();
        let leader = rig.read("sleep 5", Duration::from_millis(300));
        rig.wait_for_flight();
        let joiner = rig.read("sleep 5", Duration::from_millis(900));
        rig.wait_for_joins(1);
        rig.release();
        assert!(is_timed_out(
            leader.join().unwrap().0.expect_err("leader").message()
        ));
        let (joined, waited) = joiner.join().unwrap();
        let error = joined.expect_err("joiner deadline");
        assert!(
            is_timed_out(error.message()),
            "{error}",
            error = error.message()
        );
        assert!(
            waited < crate::test_support::coverage_relaxed(Duration::from_millis(1900)),
            "{waited:?}"
        );
        assert_eq!(
            error,
            format!("joined-read{TIMEOUT_MARKER}0.9s"),
            "a joiner's timeout names its own budget, not the remainder"
        );
    }

    /// Joiners of a leader that timed out all ask again. They ask once, as one
    /// flight: a hung git must not meet a stampede of children where it used
    /// to meet one.
    #[cfg(unix)]
    #[test]
    fn a_leaders_timeout_sends_its_joiners_to_one_new_child() {
        // Whether the unfixed code stampedes depends on how fast the first
        // rerun reaches the gate, so ask over several rounds.
        for round in 0..4 {
            let mut rig = JoinerRig::with_slots(16);
            let script = format!("echo $$ >> '{}/pids'; sleep 2", rig.cwd.display());
            let leader = rig.read(&script, Duration::from_millis(300));
            rig.wait_for_flight();
            let joiners: Vec<_> = (0..12)
                .map(|_| rig.read(&script, Duration::from_millis(1000)))
                .collect();
            rig.wait_for_joins(12);
            rig.release();
            assert!(is_timed_out(
                leader.join().unwrap().0.expect_err("leader").message()
            ));
            for joiner in joiners {
                let (joined, _) = joiner.join().unwrap();
                assert!(is_timed_out(
                    joined.expect_err("every rerun hung").message()
                ));
            }
            let started = std::fs::read_to_string(rig.cwd.join("pids")).unwrap();
            assert_eq!(
                started.lines().count(),
                2,
                "round {round}: the leader's child and one shared retry: {started}"
            );
        }
    }

    /// `coalesced` counts reads answered without a child of their own. A
    /// joiner whose leader timed out and who then ran its own child was not.
    #[cfg(unix)]
    #[test]
    fn a_joiner_that_reruns_is_not_counted_as_coalesced() {
        let mut rig = JoinerRig::new();
        let script = "if [ -e ran ]; then echo second; else touch ran; sleep 5; fi";
        let leader = rig.read(script, Duration::from_millis(300));
        rig.wait_for_flight();
        let joiner = rig.read(script, Duration::from_secs(10));
        rig.wait_for_joins(1);
        rig.release();
        assert!(is_timed_out(
            leader.join().unwrap().0.expect_err("leader").message()
        ));
        assert!(joiner.join().unwrap().0.expect("rerun").success);
        let counters = rig.gate.counters()[super::Admission::Reactive.index()];
        assert_eq!(counters.admitted, 3, "the rig's held slot and two children");
        assert_eq!(
            counters.coalesced, 0,
            "no read was answered by another's child"
        );
    }

    /// A caller that can be cancelled never takes the shared path, which has
    /// no observer to poll: a hook-running command asked for through the
    /// shared entry point still honours the cancel.
    #[cfg(unix)]
    #[test]
    fn a_hook_running_command_is_never_shared_so_it_stays_cancellable() {
        let dir = tempfile::TempDir::new().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        for args in [
            vec!["init", "-q"],
            vec!["config", "user.email", "t@example.invalid"],
            vec!["config", "user.name", "t"],
            vec!["config", "commit.gpgsign", "false"],
        ] {
            let status = Command::new("git")
                .args(&args)
                .current_dir(&repo)
                .status_locked()
                .unwrap();
            assert!(status.success(), "{args:?}");
        }
        crate::test_support::trust_repo(&repo);
        let hook = repo.join(".git/hooks/pre-commit");
        let marker = repo.join(".git/hook-started");
        std::fs::write(
            &hook,
            format!("#!/bin/sh\ntouch '{}'\nsleep 30\n", marker.display()),
        )
        .unwrap();
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let canceller = {
            let (repo, marker) = (repo.clone(), marker.clone());
            thread::spawn(move || {
                let until = Instant::now() + Duration::from_secs(20);
                while !marker.exists() {
                    assert!(Instant::now() < until, "hook never started");
                    thread::sleep(Duration::from_millis(20));
                }
                cancel_hooked_git(&repo)
            })
        };
        let started = Instant::now();
        let error =
            git_text_shared(&repo, &["commit", "--allow-empty", "-m", "x"]).expect_err("cancelled");
        assert!(
            marker.exists(),
            "the hook never ran: {error}",
            error = error.message()
        );
        assert_eq!(canceller.join().unwrap(), 1, "the run was registered");
        assert!(
            is_hooked_git_cancelled(error.message()),
            "{error}",
            error = error.message()
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(15)),
            "{:?}",
            started.elapsed()
        );
    }

    /// A rate-limit refusal after ~2s used to print the command's own 30s or
    /// 90s deadline as "timed out after 89.99s waiting for a process slot",
    /// so a launch under load read as git hanging. It must say it was
    /// deferred, and how long it actually waited.
    #[cfg(unix)]
    #[test]
    fn a_rate_limit_refusal_reports_the_wait_not_the_command_deadline() {
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::with_rate(32, 1, 1)));
        let started = Instant::now();
        let (tx, rx) = mpsc::channel();
        for _ in 0..8 {
            let tx = tx.clone();
            thread::spawn(move || {
                let result = run_with_gate(
                    &mut Command::new("/usr/bin/true"),
                    "slot-wait",
                    Duration::from_secs(30),
                    None,
                    MAX_OUTPUT_BYTES,
                    &mut (),
                    gate,
                );
                let _ = tx.send(result);
            });
        }
        drop(tx);
        let mut refusals = Vec::new();
        for _ in 0..8 {
            if let Err(error) = rx
                .recv_timeout(Duration::from_secs(10))
                .expect("queue budget")
            {
                refusals.push(error);
            }
        }
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(10)),
            "refusals sat until the command deadline: {:?}",
            started.elapsed()
        );
        assert!(
            !refusals.is_empty(),
            "a burst of 1 at 1/s cannot admit all 8 in 2s"
        );
        for error in &refusals {
            let reported: f64 = error
                .message()
                .split(" deferred under load after ")
                .nth(1)
                .and_then(|rest| rest.split('s').next())
                .and_then(|number| number.parse().ok())
                .unwrap_or_else(|| {
                    panic!("no deferral and wait in {error}", error = error.message())
                });
            assert!(
                (1.5..6.0).contains(&reported),
                "reported {reported}s from {error}; the command deadline is 30s",
                error = error.message()
            );
            assert!(
                !error.contains(TIMEOUT_MARKER),
                "a deferral is not a timeout: {error}",
                error = error.message()
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_queued_command_still_gets_its_full_runtime_after_a_slot() {
        // The queue wait is bounded by `timeout`, but spending that budget
        // in the gate used to leave the child with leftover milliseconds and
        // report "timed out" for work that had not started. Hold the only
        // slot long enough that the leftover would be less than `sleep`,
        // then prove the child still finishes.
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(1)));
        let holder = gate
            .acquire(Instant::now() + Duration::from_secs(10))
            .expect("holder");
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "sleep 0.4; echo queued-ok"]);
            let result = run_with_gate(
                &mut cmd,
                "queued-run",
                Duration::from_millis(600),
                None,
                MAX_OUTPUT_BYTES,
                &mut (),
                gate,
            );
            tx.send(result).unwrap();
        });
        thread::sleep(Duration::from_millis(350));
        drop(holder);
        let result = rx
            .recv_timeout(Duration::from_secs(5))
            .expect("queued command must start after the slot is freed")
            .expect("a command that waited in the gate must still get its full runtime");
        worker.join().unwrap();
        assert!(
            result.success,
            "stderr: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            String::from_utf8_lossy(&result.stdout).contains("queued-ok"),
            "stdout: {}",
            String::from_utf8_lossy(&result.stdout)
        );
    }

    #[cfg(unix)]
    #[test]
    fn inherited_stdin_cannot_hold_the_command_runner_after_the_parent_exits() {
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut cmd = Command::new("/bin/sh");
            cmd.args(["-c", "sleep 4 <&0 >/dev/null 2>&1 & exit 0"]);
            let result = run_with_gate(
                &mut cmd,
                "stdin-test",
                Duration::from_secs(10),
                Some(&vec![b'x'; 1024 * 1024]),
                1024,
                &mut (),
                Box::leak(Box::new(SpawnGate::new(1))),
            );
            tx.send(result).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(3));
        worker.join().unwrap();
        let error = result
            .expect("stdin must have a bounded settle window")
            .expect_err("undelivered stdin cannot report success");
        assert!(error.contains("stdin"), "{error}", error = error.message());
    }

    #[cfg(unix)]
    #[test]
    fn cancellable_stdin_delivers_large_payloads_while_stdout_drains() {
        let bytes = vec![b'q'; 8 * 1024 * 1024];
        let result = run_with_gate(
            &mut Command::new("/bin/cat"),
            "stdin-roundtrip",
            Duration::from_secs(10),
            Some(&bytes),
            bytes.len(),
            &mut (),
            Box::leak(Box::new(SpawnGate::new(1))),
        )
        .unwrap();
        assert!(result.success);
        assert!(result.incomplete.is_none());
        assert_eq!(result.stdout, bytes);
    }

    /// A permit released by a panicking holder must not leak its slot, or one
    /// failed git call would shrink the budget for the rest of the session.
    #[test]
    fn spawn_gate_reclaims_a_permit_dropped_by_a_panic() {
        let gate = std::sync::Arc::new(SpawnGate::new(1));
        let poisoner = {
            let gate = std::sync::Arc::clone(&gate);
            thread::spawn(move || {
                let _permit = gate
                    .acquire(Instant::now() + Duration::from_secs(30))
                    .unwrap();
                panic!("holder blew up mid-run");
            })
        };
        assert!(poisoner.join().is_err(), "the holder was supposed to panic");

        let (tx, rx) = mpsc::channel();
        let gate2 = std::sync::Arc::clone(&gate);
        thread::spawn(move || {
            let _permit = gate2
                .acquire(Instant::now() + Duration::from_secs(30))
                .unwrap();
            let _ = tx.send(());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the panicked holder's slot must come back");
    }

    /// Regression: nothing bounded how many children `run_bounded` kept alive
    /// at once, so a workspace-wide refresh put hundreds of `git` processes on
    /// the host simultaneously and exhausted the descriptor table -- every
    /// spawn past the ceiling failing with "Too many open files (os error
    /// 24)".
    ///
    /// Concurrency is counted by the children themselves rather than by the
    /// gate: each `sh` creates a marker file for as long as it runs, and a
    /// sampler watches how many exist at once. An assertion that read the
    /// gate's own counter could not tell a working gate from one that is
    /// never consulted, which is exactly the bug this pins.
    ///
    /// The gate is private. 64 waiters on the process-wide `spawn_gate()`
    /// starve every other short-deadline `run_bounded` test in the same
    /// binary — llvm-cov still shares one gate even with a thread cap, and a
    /// 5s slot wait then fails as "timed out waiting for a process slot"
    /// even though the drain under test is fine. Production `run_observed`
    /// always passes `spawn_gate()`; this test pins that `run_with_gate`
    /// actually limits live children.
    #[cfg(unix)]
    #[test]
    fn run_bounded_never_exceeds_the_spawn_limit_under_fan_out() {
        const CALLERS: usize = 64;
        let limit = spawn_limit();
        assert!(
            CALLERS > limit,
            "the fan-out must exceed the limit to prove anything ({CALLERS} vs {limit})"
        );
        let gate: &'static SpawnGate = Box::leak(Box::new(SpawnGate::new(limit)));

        let dir = tempfile::TempDir::new().expect("tempdir");
        let live_dir = dir.path().to_path_buf();
        let sampling = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let peak_live = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sampler = {
            let live_dir = live_dir.clone();
            let sampling = std::sync::Arc::clone(&sampling);
            let peak_live = std::sync::Arc::clone(&peak_live);
            thread::spawn(move || {
                while sampling.load(std::sync::atomic::Ordering::SeqCst) {
                    if let Ok(entries) = std::fs::read_dir(&live_dir) {
                        let n = entries.count();
                        peak_live.fetch_max(n, std::sync::atomic::Ordering::SeqCst);
                    }
                    thread::sleep(Duration::from_millis(5));
                }
            })
        };

        let ready = std::sync::Arc::new(std::sync::Barrier::new(CALLERS));
        let handles: Vec<_> = (0..CALLERS)
            .map(|i| {
                let ready = std::sync::Arc::clone(&ready);
                let marker = live_dir.join(format!("child-{i}"));
                thread::spawn(move || {
                    // Release every caller at once so they contend for real.
                    ready.wait();
                    let marker = marker.to_string_lossy().into_owned();
                    let mut cmd = Command::new("sh");
                    cmd.args([
                        "-c",
                        // Present for exactly as long as this child runs.
                        r#": > "$1"; sleep 0.3; rm -f "$1""#,
                        "sh",
                        &marker,
                    ]);
                    run_with_gate(
                        &mut cmd,
                        "sh",
                        Duration::from_secs(30),
                        None,
                        MAX_OUTPUT_BYTES,
                        &mut (),
                        gate,
                    )
                })
            })
            .collect();

        for handle in handles {
            let out = handle.join().expect("caller thread").expect("run");
            assert!(out.success, "every bounded run must still complete");
        }
        sampling.store(false, std::sync::atomic::Ordering::SeqCst);
        sampler.join().expect("sampler thread");

        let observed = peak_live.load(std::sync::atomic::Ordering::SeqCst);
        assert!(
            observed > 1,
            "the callers must genuinely have overlapped, saw {observed} at once"
        );
        assert!(
            observed <= limit,
            "at most {limit} children may be alive at once, saw {observed}"
        );
    }

    #[test]
    fn a_scrubbed_child_keeps_only_allowlisted_inherited_variables() {
        use std::ffi::OsString;
        let inherited = [
            ("PATH", "/usr/bin"),
            ("LC_ALL", "C"),
            ("HTTPS_PROXY", "http://p:1"),
            ("NPM_TOKEN", "t1"),
            ("GITHUB_TOKEN", "t2"),
            ("AWS_SESSION_TOKEN", "t3"),
            ("MY_API_KEY", "t4"),
        ]
        .map(|(k, v)| (OsString::from(k), OsString::from(v)));
        let mut cmd = Command::new("true");
        cmd.env("EXPLICIT", "yes").env_remove("PATH");
        restrict_env_to_allowlist_plus(
            &mut cmd,
            inherited,
            Some(std::ffi::OsStr::new("/h")),
            &["MY_API_KEY".to_string()],
        );
        let seen: std::collections::BTreeMap<String, Option<String>> = cmd
            .get_envs()
            .map(|(k, v)| {
                (
                    k.to_string_lossy().into_owned(),
                    v.map(|v| v.to_string_lossy().into_owned()),
                )
            })
            .collect();
        let names: Vec<&str> = seen.keys().map(String::as_str).collect();
        assert_eq!(
            names,
            ["EXPLICIT", "HOME", "HTTPS_PROXY", "LC_ALL", "MY_API_KEY"],
            "{seen:?}"
        );
        // A PATH the builder removed is not resurrected from the inherited one.
        assert!(!seen.contains_key("PATH"));
        assert_eq!(seen["HOME"].as_deref(), Some("/h"));
    }

    #[test]
    fn sandbox_join_canonical_resolves_and_stays_inside() {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src").join("main.rs"), "fn main() {}").unwrap();
        let repo = dir.path().canonicalize().unwrap();

        // Existing nested file resolves through to the canonical location.
        let resolved = sandbox_join_canonical(&repo, "src/main.rs").expect("existing nested file");
        assert_eq!(resolved, repo.join("src").join("main.rs"));

        // Non-existent trailing components stay lexical so new files can be
        // created in new nested directories.
        let fresh = sandbox_join_canonical(&repo, "deep/new/dir/file.txt").expect("new file");
        assert_eq!(
            fresh,
            repo.join("deep").join("new").join("dir").join("file.txt")
        );

        // Lexical escapes are still rejected before any filesystem work.
        assert!(sandbox_join_canonical(&repo, "../outside").is_err());
        assert!(sandbox_join_canonical(&repo, "/etc/passwd").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn sandbox_join_canonical_refuses_symlink_escape() {
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

        let dir = tempfile::TempDir::new().unwrap();
        std::os::unix::fs::symlink(outside.path().join("secret.txt"), dir.path().join("leak"))
            .unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("dir-leak")).unwrap();
        let repo = dir.path().canonicalize().unwrap();

        let err = sandbox_join_canonical(&repo, "leak").expect_err("file symlink escape");
        assert!(is_sandbox_symlink_escape(&err), "got: {err}");
        assert!(err.contains("escapes the repository"), "got: {err}");
        let err = sandbox_join_canonical(&repo, "dir-leak/payload.txt")
            .expect_err("directory symlink escape");
        assert!(err.contains("escapes the repository"), "got: {err}");

        // A dangling symlink pointing outside must also be refused, not passed
        // through as a merely "non-existent" trailing component.
        std::os::unix::fs::symlink(
            outside.path().join("does-not-exist.txt"),
            dir.path().join("dangling"),
        )
        .unwrap();
        assert!(sandbox_join_canonical(&repo, "dangling").is_err());
        // ...and a symlink that stays inside the repo is fine.
        std::fs::write(dir.path().join("inside.txt"), "ok").unwrap();
        std::os::unix::fs::symlink(dir.path().join("inside.txt"), dir.path().join("alias"))
            .unwrap();
        assert_eq!(
            sandbox_join_canonical(&repo, "alias").expect("internal symlink"),
            repo.join("inside.txt")
        );
    }

    #[test]
    fn sandbox_write_survives_non_canonical_repo_path() {
        // On macOS TempDir lives under /var -> /private/var; validate_repo
        // canonicalizes, and sandbox_join_canonical canonicalizes both sides
        // again, so prefix comparison never mixes the two spellings.
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        sandbox_write(&raw, "notes/todo.md", "- item").expect("write via raw path");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes").join("todo.md")).unwrap(),
            "- item"
        );
    }

    /// Regression for the symlinked-directory escape: `create_dir_all` +
    /// `fs::write` used to follow a repo-internal symlink and land the file
    /// outside the repository. The canonicalizing join must refuse it while
    /// ordinary writes keep working.
    #[cfg(unix)]
    #[test]
    fn sandbox_write_refuses_symlinked_directory_escape() {
        let outside = tempfile::TempDir::new().unwrap();
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();

        let raw = dir.path().to_string_lossy().into_owned();
        let err = sandbox_write(&raw, "link/sub.txt", "escaped")
            .expect_err("write through a directory symlink must fail");
        assert!(err.contains("escapes"), "got: {err}", err = err.message());
        assert!(
            !outside.path().join("sub.txt").exists(),
            "nothing may be written through the link"
        );

        // A plain path still writes normally.
        sandbox_write(&raw, "real/sub.txt", "kept inside").expect("direct write");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("real").join("sub.txt")).unwrap(),
            "kept inside"
        );
    }

    /// What the file editor's `<textarea>` hands back: the HTML spec's value
    /// sanitization turns every CRLF pair and every lone CR into LF.
    fn textarea_value(text: &str) -> String {
        text.replace("\r\n", "\n").replace('\r', "\n")
    }

    /// Open → edit one line → save, exactly as the file viewer does it.
    fn editor_round_trip(
        dir: &Path,
        file: &str,
        edit: impl Fn(&str) -> String,
    ) -> Result<(), CommandError> {
        let raw = dir.to_string_lossy().into_owned();
        let blob = crate::engine::GitReader::get_file_blob(&raw, file, None)?;
        let shown = textarea_value(blob.text.as_deref().expect("text file"));
        sandbox_write_edited_text(&raw, file, &edit(&shown)).map_err(CommandError::from)
    }

    #[test]
    fn editor_round_trip_keeps_crlf_outside_and_inside_the_edit() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("win.txt"), b"one\r\ntwo\r\nthree\r\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let blob = crate::engine::GitReader::get_file_blob(&raw, "win.txt", None).unwrap();
        assert_eq!(blob.eol, Some(crate::engine::text_shape::LineEnding::Crlf));
        assert_eq!(blob.invalid_utf8_bytes, 0);

        editor_round_trip(dir.path(), "win.txt", |text| {
            text.replace("two", "TWO\nand more")
        })
        .expect("save");

        assert_eq!(
            std::fs::read(dir.path().join("win.txt")).unwrap(),
            b"one\r\nTWO\r\nand more\r\nthree\r\n"
        );
    }

    #[test]
    fn editor_round_trip_keeps_lf_files_byte_exact() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("unix.txt"), "one\ntwo\ncafé\n").unwrap();

        editor_round_trip(dir.path(), "unix.txt", |text| text.replace("two", "TWO")).expect("save");

        assert_eq!(
            std::fs::read(dir.path().join("unix.txt")).unwrap(),
            "one\nTWO\ncafé\n".as_bytes()
        );
    }

    #[test]
    fn editor_round_trip_never_rewrites_latin1_bytes() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let original = b"caf\xe9\nna\xefve\n".to_vec();
        std::fs::write(dir.path().join("latin1.txt"), &original).unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let blob = crate::engine::GitReader::get_file_blob(&raw, "latin1.txt", None).unwrap();
        assert_eq!(
            blob.invalid_utf8_bytes, 2,
            "the reader must report the lossy decode"
        );

        let err = editor_round_trip(dir.path(), "latin1.txt", |text| text.replace("na", "NA"))
            .expect_err("a lossy decode must not be saved");
        assert!(
            err.contains("2 byte(s)") && err.contains("not valid UTF-8"),
            "got: {err}",
            err = err.message()
        );

        assert_eq!(
            std::fs::read(dir.path().join("latin1.txt")).unwrap(),
            original,
            "a lossy decode must never be written back over the original bytes"
        );
    }

    #[test]
    fn editor_round_trip_refuses_mixed_line_endings_untouched() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let original = b"one\r\ntwo\nthree\r\n".to_vec();
        std::fs::write(dir.path().join("mixed.txt"), &original).unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let blob = crate::engine::GitReader::get_file_blob(&raw, "mixed.txt", None).unwrap();
        assert_eq!(blob.eol, Some(crate::engine::text_shape::LineEnding::Mixed));

        let err = editor_round_trip(dir.path(), "mixed.txt", |text| text.replace("two", "TWO"))
            .expect_err("mixed endings cannot be kept per line");
        assert!(
            err.contains("mixes line endings"),
            "got: {err}",
            err = err.message()
        );
        assert_eq!(
            std::fs::read(dir.path().join("mixed.txt")).unwrap(),
            original
        );
    }

    #[test]
    fn edited_text_write_still_creates_new_files() {
        // The file tree's New File / New Folder (.gitkeep) go through the same
        // command with empty content and no existing file.
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        sandbox_write_edited_text(&raw, "pkg/.gitkeep", "").expect("create");
        assert_eq!(std::fs::read(dir.path().join("pkg/.gitkeep")).unwrap(), b"");
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        std::fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn create_refuses_existing_tracked_untracked_ignored_lf_crlf_empty() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let root = dir.path();
        let raw = root.to_string_lossy().into_owned();
        std::fs::write(root.join(".gitignore"), "ignored-*\n").unwrap();
        let shapes: [(&str, &[u8]); 3] =
            [("lf", b"a\nb\n"), ("crlf", b"a\r\nb\r\n"), ("empty", b"")];
        let mut cases = Vec::new();
        for (shape, bytes) in shapes {
            for kind in ["tracked", "untracked", "ignored"] {
                let name = if kind == "ignored" {
                    format!("ignored-{shape}.txt")
                } else {
                    format!("{kind}-{shape}.txt")
                };
                std::fs::write(root.join(&name), bytes).unwrap();
                if kind == "tracked" {
                    git_text(root, &["add", "--", &name]).unwrap();
                }
                let before = mtime(&root.join(&name));
                cases.push((name, bytes, before));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
        for (name, bytes, before) in cases {
            let err = sandbox_create(&raw, &name).expect_err(&name);
            assert!(
                matches!(err, FileWriteError::AlreadyExists { .. }),
                "{name}: {err:?}"
            );
            assert_eq!(std::fs::read(root.join(&name)).unwrap(), bytes, "{name}");
            assert_eq!(mtime(&root.join(&name)), before, "{name} mtime");
        }
    }

    #[test]
    fn create_refuses_dir_and_symlink() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let root = dir.path();
        let raw = root.to_string_lossy().into_owned();
        std::fs::create_dir(root.join("folder")).unwrap();
        std::fs::write(root.join("target.txt"), "keep").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink("target.txt", root.join("link.txt")).unwrap();
        for name in ["folder", "link.txt"] {
            if !root.join(name).exists() && std::fs::symlink_metadata(root.join(name)).is_err() {
                continue;
            }
            let err = sandbox_create(&raw, name).expect_err(name);
            assert!(
                matches!(err, FileWriteError::AlreadyExists { .. }),
                "{name}: {err:?}"
            );
        }
        assert_eq!(std::fs::read(root.join("target.txt")).unwrap(), b"keep");
    }

    #[test]
    fn create_makes_an_empty_file_and_its_folders() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        sandbox_create(&raw, "a/b/new.txt").expect("create");
        assert_eq!(std::fs::read(dir.path().join("a/b/new.txt")).unwrap(), b"");
    }

    #[test]
    fn new_folder_keeps_nonempty_gitkeep() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::create_dir(dir.path().join("pkg")).unwrap();
        std::fs::write(dir.path().join("pkg/.gitkeep"), "keep me\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let err = sandbox_create(&raw, "pkg/.gitkeep").expect_err("exists");
        assert!(
            matches!(err, FileWriteError::AlreadyExists { .. }),
            "{err:?}"
        );
        assert_eq!(
            std::fs::read(dir.path().join("pkg/.gitkeep")).unwrap(),
            b"keep me\n"
        );
    }

    fn open_base(raw: &str, file: &str) -> String {
        crate::engine::GitReader::get_file_blob(raw, file, None)
            .unwrap()
            .disk_base
            .expect("a working-tree read carries a base")
    }

    #[test]
    fn save_with_stale_base_is_refused_and_disk_keeps_external_edit() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let base = open_base(&raw, "a.txt");
        std::fs::write(dir.path().join("a.txt"), "agent wrote this\n").unwrap();
        let err = sandbox_save_edited_text(&raw, "a.txt", "my draft\n", Some(&base))
            .expect_err("stale base");
        match err {
            FileWriteError::ChangedOnDisk {
                expected, actual, ..
            } => {
                assert_eq!(expected, base);
                assert_eq!(actual, open_base(&raw, "a.txt"));
            }
            other => panic!("expected changed_on_disk, got {other:?}"),
        }
        assert_eq!(
            std::fs::read(dir.path().join("a.txt")).unwrap(),
            b"agent wrote this\n"
        );
    }

    #[test]
    fn save_with_current_base_succeeds() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let base = open_base(&raw, "a.txt");
        sandbox_save_edited_text(&raw, "a.txt", "two\n", Some(&base)).expect("current base");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"two\n");
    }

    #[test]
    fn saved_returns_new_base() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let base = open_base(&raw, "a.txt");
        let saved = sandbox_save_edited_text(&raw, "a.txt", "two\n", Some(&base)).unwrap();
        assert_eq!(saved.disk_base, open_base(&raw, "a.txt"));
        // The returned base is the one the next save of the same draft carries.
        sandbox_save_edited_text(&raw, "a.txt", "three\n", Some(&saved.disk_base))
            .expect("chained save");
    }

    #[test]
    fn view_only_reads_skip_the_base_and_the_editor_open_keeps_it() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let open = crate::engine::GitReader::get_file_blob(&raw, "a.txt", None).unwrap();
        assert!(
            open.disk_base.is_some(),
            "the editor open must carry a base"
        );
        let view = crate::engine::GitReader::get_file_blob_view(&raw, "a.txt", None).unwrap();
        assert_eq!(
            view.disk_base, None,
            "a view-only read must not pay for one"
        );
        assert_eq!(view.text, open.text);
        assert_eq!(
            crate::engine::GitReader::get_file_content(&raw, "a.txt", None).unwrap(),
            "one\n"
        );
    }

    #[test]
    fn crlf_base_round_trips() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("w.txt"), "a\r\nb\r\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let base = open_base(&raw, "w.txt");
        let saved = sandbox_save_edited_text(&raw, "w.txt", "a\nB\n", Some(&base)).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("w.txt")).unwrap(),
            b"a\r\nB\r\n"
        );
        assert_eq!(saved.disk_base, open_base(&raw, "w.txt"));
    }

    #[test]
    fn a_file_renamed_while_open_reports_absent_and_overwrite_recreates_it() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("a.txt"), "one\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let base = open_base(&raw, "a.txt");
        std::fs::rename(dir.path().join("a.txt"), dir.path().join("b.txt")).unwrap();
        let err = sandbox_save_edited_text(&raw, "a.txt", "x\n", Some(&base)).expect_err("gone");
        let FileWriteError::ChangedOnDisk { actual, .. } = err else {
            panic!("expected changed_on_disk, got {err:?}");
        };
        assert_eq!(actual, "absent");
        sandbox_save_edited_text(&raw, "a.txt", "x\n", Some(&actual)).expect("overwrite");
        assert_eq!(std::fs::read(dir.path().join("a.txt")).unwrap(), b"x\n");
    }

    #[test]
    fn file_write_error_serializes_to_the_contract_shape() {
        let keys = |error: FileWriteError| -> (String, Vec<String>) {
            let value = serde_json::to_value(&error).unwrap();
            let object = value.as_object().unwrap();
            let mut keys: Vec<String> = object.keys().cloned().collect();
            keys.sort();
            (object["code"].as_str().unwrap().to_string(), keys)
        };
        let strings = |names: &[&str]| names.iter().map(|n| n.to_string()).collect::<Vec<_>>();
        assert_eq!(
            keys(FileWriteError::AlreadyExists {
                message: "m".into(),
                path: "p".into()
            }),
            (
                "already_exists".to_string(),
                strings(&["code", "message", "path"])
            )
        );
        assert_eq!(
            keys(FileWriteError::ChangedOnDisk {
                message: "m".into(),
                path: "p".into(),
                expected: "e".into(),
                actual: "a".into()
            }),
            (
                "changed_on_disk".to_string(),
                strings(&["actual", "code", "expected", "message", "path"])
            )
        );
        assert_eq!(
            keys(FileWriteError::from("no".to_string())),
            ("refused".to_string(), strings(&["code", "message"]))
        );
        let saved = serde_json::to_value(SavedFile {
            disk_base: "x".into(),
        })
        .unwrap();
        assert_eq!(saved, serde_json::json!({ "disk_base": "x" }));
    }

    #[test]
    fn git_metadata_components_are_recognised_and_lookalikes_are_not() {
        use std::ffi::OsStr;
        for name in [
            ".git",
            ".GIT",
            ".Git",
            ".git.",
            ".git ",
            ".git. .",
            ".git:stream",
            ".git::$INDEX_ALLOCATION",
        ] {
            assert!(is_git_metadata_component(OsStr::new(name)), "{name}");
        }
        for name in [
            ".gitignore",
            ".gitkeep",
            ".github",
            ".gitattributes",
            ".gitmodules",
            "git",
            "x.git",
            "git~1x",
            "..git",
            "",
        ] {
            assert!(!is_git_metadata_component(OsStr::new(name)), "{name}");
        }
        // Refused on every platform, as git's `is_ntfs_dotgit` does by default.
        assert!(is_git_metadata_component(OsStr::new("GIT~1")));
        for path in [".git/config", "a/.git/hooks/x", "sub/.GIT", "a/.git./b"] {
            assert!(sandbox_join(Path::new("/r"), path).is_err(), "{path}");
        }
        for path in [".gitignore", ".github/x", "sub/.gitattributes"] {
            assert!(sandbox_join(Path::new("/r"), path).is_ok(), "{path}");
        }
    }

    /// Each row is a spelling git's `verify_path` refuses in a checkout, and
    /// the git function (read from git's `utf8.c` / `path.c`) that refuses it.
    #[test]
    fn every_dotgit_spelling_git_verify_path_refuses_is_refused() {
        use std::ffi::OsStr;
        let rows: &[(&str, &str)] = &[
            // is_hfs_dotgit / next_hfs_char `ignorable`: one code point at each
            // position, for every code point git lists.
            (".g\u{200c}it", "is_hfs_dotgit: U+200C"),
            (".g\u{200d}it", "is_hfs_dotgit: U+200D"),
            (".g\u{200e}it", "is_hfs_dotgit: U+200E"),
            (".g\u{200f}it", "is_hfs_dotgit: U+200F"),
            (".g\u{202a}it", "is_hfs_dotgit: U+202A"),
            (".g\u{202b}it", "is_hfs_dotgit: U+202B"),
            (".g\u{202c}it", "is_hfs_dotgit: U+202C"),
            (".g\u{202d}it", "is_hfs_dotgit: U+202D"),
            (".g\u{202e}it", "is_hfs_dotgit: U+202E"),
            (".g\u{206a}it", "is_hfs_dotgit: U+206A"),
            (".g\u{206b}it", "is_hfs_dotgit: U+206B"),
            (".g\u{206c}it", "is_hfs_dotgit: U+206C"),
            (".g\u{206d}it", "is_hfs_dotgit: U+206D"),
            (".g\u{206e}it", "is_hfs_dotgit: U+206E"),
            (".g\u{206f}it", "is_hfs_dotgit: U+206F"),
            (".g\u{feff}it", "is_hfs_dotgit: U+FEFF"),
            (".\u{feff}git", "is_hfs_dotgit: ignorable after the dot"),
            ("\u{200c}.git", "is_hfs_dotgit: ignorable before the dot"),
            (".git\u{200d}", "is_hfs_dotgit: ignorable at the end"),
            (".\u{200c}g\u{200d}i\u{200e}t", "is_hfs_dotgit: several"),
            (".G\u{200c}IT", "is_hfs_dotgit: case folded"),
            // is_ntfs_dotgit.
            (".GIT", "is_ntfs_dotgit: case folded"),
            (".git.", "is_ntfs_dotgit: trailing dot"),
            (".git ", "is_ntfs_dotgit: trailing space"),
            (".git. .", "is_ntfs_dotgit: trailing dots and spaces"),
            (".git:stream", "is_ntfs_dotgit: stream suffix"),
            (
                ".git::$INDEX_ALLOCATION",
                "is_ntfs_dotgit: directory stream",
            ),
            ("git~1", "is_ntfs_dotgit: 8.3 short name"),
            ("GIT~1", "is_ntfs_dotgit: 8.3 short name, case folded"),
            ("git~1 ", "is_ntfs_dotgit: short name, trailing space"),
            ("git~1:$DATA", "is_ntfs_dotgit: short name, stream"),
            // Both at once: an HFS-ignorable inside an NTFS-padded name.
            (".g\u{200c}it. ", "is_hfs_dotgit + is_ntfs_dotgit"),
        ];
        for (name, mirrors) in rows {
            assert!(
                is_git_metadata_component(OsStr::new(name)),
                "{name:?} must be refused ({mirrors})"
            );
            assert!(
                sandbox_join(Path::new("/r"), &format!("sub/{name}/config")).is_err(),
                "{name:?} as a directory ({mirrors})"
            );
        }
        // Names git accepts, or that only look close: not on git's list.
        for name in [
            ".g\u{200b}it",  // U+200B is not in `ignorable`
            ".gi\u{0307}t",  // a combining mark is not ignorable
            ".gitx\u{200c}", // ignorable removed leaves `.gitx`
            "git~2",
            "git~10",
            "\u{200c}",
        ] {
            assert!(!is_git_metadata_component(OsStr::new(name)), "{name:?}");
        }
    }

    /// A nested repository whose `.git` FILE names a gitdir elsewhere in the
    /// tree: `sub/.git` -> `gitdir: ../.modules/sub`.
    fn nested_repo_with_separate_gitdir() -> (tempfile::TempDir, String) {
        use crate::procguard::LockedSpawn;
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        init_plain_git_repo(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::create_dir_all(root.join(".modules")).unwrap();
        std::fs::write(root.join("sub/lib.txt"), "x\n").unwrap();
        let output = std::process::Command::new("git")
            .args(["init", "-q", "--separate-git-dir", "../.modules/sub"])
            .current_dir(root.join("sub"))
            .output_locked()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
        assert!(root.join("sub/.git").is_file());
        assert!(root.join(".modules/sub/HEAD").is_file());
        let raw = root.to_string_lossy().into_owned();
        (dir, raw)
    }

    #[test]
    fn a_nested_repositorys_real_git_directory_is_refused_for_every_write() {
        let (_dir, raw) = nested_repo_with_separate_gitdir();
        let repo = Path::new(&raw);
        let before = std::fs::read(repo.join(".modules/sub/HEAD")).unwrap();
        for target in [
            ".modules/sub/HEAD",
            ".modules/sub/config",
            ".modules/sub/hooks/pre-commit",
            ".modules/sub/objects/ab/new",
            ".modules/sub",
        ] {
            assert!(sandbox_join_write(repo, target).is_err(), "write {target}");
            assert!(
                sandbox_join_write_entry(repo, target).is_err(),
                "entry (create, move, delete) {target}"
            );
            assert!(sandbox_create(&raw, target).is_err(), "create {target}");
            assert!(sandbox_write(&raw, target, "x").is_err(), "save {target}");
        }
        assert_eq!(
            std::fs::read(repo.join(".modules/sub/HEAD")).unwrap(),
            before,
            "nothing was written into the nested gitdir"
        );
        assert!(!repo.join(".modules/sub/hooks/pre-commit").exists());
        // A neighbouring ordinary file and the nested work tree stay writable.
        for target in ["sub/lib.txt", ".modules/notes.txt", "sub/new.txt"] {
            sandbox_join_write(repo, target)
                .unwrap_or_else(|e| panic!("{target}: {e}", e = e.message()));
        }
        sandbox_write(&raw, "sub/lib.txt", "y\n").unwrap();
    }

    /// A backslash is a separator to git (`is_dir_sep`, CVE-2019-1354 shape).
    #[test]
    fn a_backslash_separated_dotgit_piece_is_refused_in_any_position() {
        use std::ffi::OsStr;
        for name in [
            ".git\\config",
            ".git\\hooks\\pre-commit",
            "git~1\\x",
            ".GIT\\x",
            ".git .\\x",
            "a\\.git\\x",
            "a\\b\\.git",
            ".g\u{200c}it\\x",
            "\\.git",
        ] {
            assert!(is_git_metadata_component(OsStr::new(name)), "{name:?}");
        }
        for name in ["a\\b", "x\\.gitignore", ".gitx\\y", "\\", "a\\git~2"] {
            assert!(!is_git_metadata_component(OsStr::new(name)), "{name:?}");
        }
        assert!(is_gitmodules_component(OsStr::new(".gitmodules")));
        assert!(is_gitmodules_component(OsStr::new("GITMOD~1")));
        assert!(is_gitmodules_component(OsStr::new(".gitmod\u{200d}ules. ")));
        assert!(!is_gitmodules_component(OsStr::new(".gitmodules2")));
        assert!(!is_gitmodules_component(OsStr::new(".gitignore")));
    }

    #[test]
    fn the_downward_walk_finds_a_nested_gitdir_and_names_where() {
        let (_dir, raw) = nested_repo_with_separate_gitdir();
        let repo = Path::new(&raw);
        let err = refuse_nested_gitdir_below(&repo.join(".modules"), ".modules").unwrap_err();
        assert!(
            err.contains("contains the git directory of a nested repository at .modules/sub"),
            "{err}"
        );
        // A work tree whose `.git` is a file has no gitdir below it; one whose
        // `.git` is a directory carries it along and is not refused.
        assert!(refuse_nested_gitdir_below(&repo.join("sub"), "sub").is_ok());
        std::fs::create_dir_all(repo.join("full")).unwrap();
        init_plain_git_repo(&repo.join("full"));
        assert!(refuse_nested_gitdir_below(&repo.join("full"), "full").is_ok());
        // A file has nothing below it.
        assert!(refuse_nested_gitdir_below(&repo.join("sub/lib.txt"), "sub/lib.txt").is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn the_downward_walk_never_follows_a_symlink() {
        let (_dir, raw) = nested_repo_with_separate_gitdir();
        let repo = Path::new(&raw);
        std::fs::create_dir_all(repo.join("only-links")).unwrap();
        std::os::unix::fs::symlink(repo.join(".modules"), repo.join("only-links/l")).unwrap();
        assert!(refuse_nested_gitdir_below(&repo.join("only-links"), "only-links").is_ok());
        // A link to the gitdir itself is the entry, not a gitdir.
        std::os::unix::fs::symlink(repo.join(".modules/sub"), repo.join("lnk")).unwrap();
        assert!(refuse_nested_gitdir_below(&repo.join("lnk"), "lnk").is_ok());
    }

    /// A walk cut short by either cap refuses; it never reads as clean.
    #[test]
    fn the_downward_walk_refuses_when_a_cap_is_hit() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for n in 0..10 {
            std::fs::write(root.join(format!("f{n}")), "").unwrap();
        }
        assert!(refuse_nested_gitdir_below_capped(&root, "d", 10, 4).is_ok());
        let err = refuse_nested_gitdir_below_capped(&root, "d", 9, 4).unwrap_err();
        assert!(err.contains("too large to check"), "{err}");
        let deep = root.join("a/b/c/d/e");
        std::fs::create_dir_all(&deep).unwrap();
        let err = refuse_nested_gitdir_below_capped(&root, "d", 1000, 3).unwrap_err();
        assert!(err.contains("too large to check"), "{err}");
        assert!(refuse_nested_gitdir_below_capped(&root, "d", 1000, 5).is_ok());
    }

    /// Cost of the walk over a 10,000-entry folder, printed for the report
    /// (`--nocapture`). No wall-clock assertion: the number depends on the host.
    #[test]
    fn the_downward_walk_over_ten_thousand_entries_completes_clean() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for d in 0..100 {
            let sub = root.join(format!("d{d}"));
            std::fs::create_dir(&sub).unwrap();
            for f in 0..99 {
                std::fs::write(sub.join(format!("f{f}")), "").unwrap();
            }
        }
        let started = std::time::Instant::now();
        refuse_nested_gitdir_below(&root, "big").expect("10k entries are within the cap");
        eprintln!("walk of 10,000 entries: {:?}", started.elapsed());
    }

    #[test]
    fn a_nested_gitdir_is_found_at_any_depth_and_under_any_name() {
        let dir = tempfile::TempDir::new().unwrap();
        let root = dir.path().canonicalize().unwrap();
        init_plain_git_repo(&root);
        let deep = root.join("a/b/c/store.d");
        std::fs::create_dir_all(deep.join("refs")).unwrap();
        std::fs::create_dir_all(deep.join("objects")).unwrap();
        std::fs::write(deep.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        // A `.git` file elsewhere naming it, and one naming a gitdir outside
        // the repository entirely.
        std::fs::create_dir_all(root.join("x")).unwrap();
        std::fs::write(root.join("x/.git"), "gitdir: ../a/b/c/store.d\n").unwrap();
        let outside = tempfile::TempDir::new().unwrap();
        std::fs::create_dir_all(root.join("y")).unwrap();
        std::fs::write(
            root.join("y/.git"),
            format!("gitdir: {}\n", outside.path().display()),
        )
        .unwrap();
        let raw = root.to_string_lossy().into_owned();
        assert!(sandbox_join_write(&root, "a/b/c/store.d/hooks/post-commit").is_err());
        assert!(sandbox_join_write_entry(&root, "a/b/c/store.d/refs/heads/main").is_err());
        assert!(sandbox_join_write(&root, "a/b/c/other.txt").is_ok());
        // The `.git` files themselves are refused lexically, whatever they name.
        for file in ["x/.git", "y/.git"] {
            assert!(sandbox_join_write(&root, file).is_err(), "{file}");
        }
        assert!(sandbox_create(&raw, "a/b/c/store.d/new").is_err());
        // The repository root is never taken for a git directory, so a
        // bare-layout checkout is not locked whole.
        assert!(!inside_nested_git_directory(&root, &root.join("a")));
    }

    /// Overwrite keeps the user's buffer when the encoder cannot round-trip
    /// what is on disk, and an ordinary save of the same file is still refused.
    #[test]
    fn overwrite_writes_the_buffer_byte_exactly_over_unencodable_disk_files() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        for (name, on_disk) in [
            ("mixed.txt", b"a\r\nb\nc\r\n".as_slice()),
            ("latin1.txt", b"caf\xe9\n".as_slice()),
        ] {
            std::fs::write(dir.path().join(name), on_disk).unwrap();
            let base = open_base(&raw, name);
            let refused = sandbox_save_edited_text(&raw, name, "mine\r\nline\n", Some(&base))
                .expect_err("an ordinary save is still refused");
            assert!(
                matches!(refused, FileWriteError::Refused { .. }),
                "{refused:?}"
            );
            assert_eq!(std::fs::read(dir.path().join(name)).unwrap(), on_disk);
            let saved = sandbox_overwrite_edited_text(&raw, name, "mine\r\nline\n", Some(&base))
                .unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(
                std::fs::read(dir.path().join(name)).unwrap(),
                b"mine\r\nline\n",
                "{name}: the buffer, byte for byte"
            );
            assert_eq!(saved.disk_base, open_base(&raw, name));
        }
    }

    #[test]
    fn overwrite_still_judges_the_base_and_keeps_crlf_for_a_clean_crlf_file() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        let raw = dir.path().to_string_lossy().into_owned();
        std::fs::write(dir.path().join("w.txt"), "a\r\nb\r\n").unwrap();
        let base = open_base(&raw, "w.txt");
        std::fs::write(dir.path().join("w.txt"), "moved\r\n").unwrap();
        let err = sandbox_overwrite_edited_text(&raw, "w.txt", "x\n", Some(&base))
            .expect_err("a stale base is refused");
        assert!(
            matches!(err, FileWriteError::ChangedOnDisk { .. }),
            "{err:?}"
        );
        let current = open_base(&raw, "w.txt");
        sandbox_overwrite_edited_text(&raw, "w.txt", "x\ny\n", Some(&current)).unwrap();
        assert_eq!(
            std::fs::read(dir.path().join("w.txt")).unwrap(),
            b"x\r\ny\r\n"
        );
        assert!(sandbox_overwrite_edited_text(&raw, ".git/config", "x", None).is_err());
    }

    #[test]
    fn get_file_blob_reports_no_text_shape_for_binary_files() {
        let dir = tempfile::TempDir::new().unwrap();
        init_plain_git_repo(dir.path());
        std::fs::write(dir.path().join("data.bin"), b"\x00\xff\r\n").unwrap();
        let raw = dir.path().to_string_lossy().into_owned();
        let blob = crate::engine::GitReader::get_file_blob(&raw, "data.bin", None).unwrap();
        assert!(blob.is_binary);
        assert_eq!((blob.invalid_utf8_bytes, blob.eol), (0, None));
    }

    #[test]
    fn clamp_failure_message_cuts_on_character_boundaries() {
        let short = "éééé"; // each é is 2 bytes
        assert_eq!(clamp_failure_message(short, "push"), short);
        // Odd budgets land mid-character at both cut points.
        let long = format!("x{}", "é".repeat(MAX_FAILURE_MESSAGE_BYTES));
        let clamped = clamp_failure_message(&long, "push");
        assert!(!clamped.contains('\u{FFFD}'));
        assert!(clamped.len() < MAX_FAILURE_MESSAGE_BYTES + 100);
        assert!(clamped.contains("bytes elided (git push output truncated)"));
    }

    /// A pre-push hook prints its banners and warnings first and the failing
    /// check last; the clamp must keep the last line, not only the first.
    #[test]
    fn clamp_failure_message_keeps_the_failing_tail() {
        let chatter = "stderr | Received `false` for a non-boolean attribute\n".repeat(200);
        let diagnosis =
            format!("$ node scripts/local-ci.js\n{chatter}FAIL src/a.test.ts > breaks\nerror: failed to push some refs");
        let clamped = clamp_failure_message(&diagnosis, "push");
        assert!(clamped.starts_with("$ node scripts/local-ci.js"));
        assert!(clamped.ends_with("FAIL src/a.test.ts > breaks\nerror: failed to push some refs"));
        let elided: usize = clamped
            .split("… ")
            .nth(1)
            .and_then(|rest| rest.split(' ').next())
            .and_then(|n| n.parse().ok())
            .expect("elision notice names a byte count");
        let kept = clamped.len()
            - clamped
                .lines()
                .find(|l| l.contains("elided"))
                .unwrap()
                .len()
            - 2;
        assert_eq!(kept + elided, diagnosis.len());
    }

    fn init_plain_git_repo(dir: &Path) {
        let output = std::process::Command::new("git")
            .arg("init")
            .current_dir(dir)
            .output_locked()
            .expect("spawn git init");
        assert!(output.status.success());
        crate::test_support::trust_repo(dir);
    }

    /// THE CLASS: a count too large for its type used to read as zero, which a
    /// caller cannot tell apart from a genuinely empty result. Every one of
    /// these parsed git's own output, so the input is not hypothetical — it is
    /// whatever a big enough repository produces.
    #[test]
    fn oversized_counts_saturate_instead_of_reading_as_zero() {
        let huge = "9".repeat(40);
        assert_eq!(parse_count_saturating(&huge), Some(usize::MAX));
        assert_eq!(parse_u64_saturating(&huge), Some(u64::MAX));

        // Ahead/behind: zero here renders a wildly diverged branch "in sync".
        let (behind, ahead) = parse_left_right_count(&format!("{huge}\t{huge}"));
        assert_eq!(behind, usize::MAX);
        assert_eq!(ahead, usize::MAX);
    }

    /// The other half of the rule: text that is not a number keeps whatever
    /// zero-ish meaning it already had. `git diff --numstat` writes `-` for a
    /// binary file, and there zero is the correct answer — saturating that to
    /// `usize::MAX` would turn every binary file into the largest diff in the
    /// repository.
    #[test]
    fn non_numeric_counts_stay_absent_rather_than_saturating() {
        assert_eq!(parse_count_saturating("-"), None);
        assert_eq!(parse_count_saturating(""), None);
        assert_eq!(parse_count_saturating("   "), None);
        assert_eq!(parse_count_saturating("abc"), None);
        assert_eq!(parse_u64_saturating("-"), None);

        // A missing side of the ahead/behind pair is still zero, not MAX.
        assert_eq!(parse_left_right_count(""), (0, 0));
        assert_eq!(parse_left_right_count("-\t-"), (0, 0));
    }

    #[test]
    fn ordinary_counts_are_unchanged() {
        assert_eq!(parse_count_saturating("42"), Some(42));
        assert_eq!(parse_count_saturating(" 7 "), Some(7));
        assert_eq!(parse_u64_saturating("1024"), Some(1024));
        assert_eq!(parse_left_right_count("3\t9"), (3, 9));
        assert_eq!(parse_left_right_count("3 9"), (3, 9));
    }

    #[test]
    #[cfg(unix)]
    fn audit_full_duplex_stress_preserves_bytes_and_deadline() {
        let payload = vec![b'x'; 2 * 1024 * 1024];
        for _ in 0..8 {
            let mut cmd = Command::new("sh");
            cmd.args(["-c", "head -c 2000000 /dev/zero >&2 & cat; wait"]);
            let out = run_bounded(cmd, "duplex", Duration::from_secs(10), Some(&payload)).unwrap();
            assert!(out.success);
            assert!(out.incomplete.is_none() && out.stderr_incomplete.is_none());
            assert_eq!(out.stdout, payload);
            assert_eq!(out.stderr, vec![0; 2_000_000]);
        }
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "while :; do head -c 65536 /dev/zero; done"]);
        let started = Instant::now();
        assert!(
            run_bounded_capped(cmd, "hot-output", Duration::from_millis(100), None, 1024)
                .unwrap_err()
                .contains("timed out")
        );
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    }

    /// A child that floods stdout and, once asked to stop, writes a large
    /// cleanup burst before releasing its marker. The marker is only removed
    /// if the run keeps reading its pipes while the child is being asked.
    #[cfg(unix)]
    const GRACE_FLOODER: &str = r#"
        trap 'head -c 80000 /dev/zero; echo cleaned >&2; rm -f "$1"; exit 0' TERM
        touch "$1"
        while :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done
    "#;

    #[cfg(unix)]
    fn grace_flooder(script: &str, marker: &Path) -> Command {
        let mut cmd = Command::new("sh");
        cmd.arg("-c").arg(script).arg("sh").arg(marker);
        cmd
    }

    /// R2-02: the grace after a deadline reads the pipes, so a child blocked
    /// on a full pipe can run its SIGTERM cleanup instead of being killed.
    #[cfg(unix)]
    #[test]
    fn a_deadline_grace_keeps_draining_so_cleanup_after_sigterm_completes() {
        let _kept = KeepDeadlinePrefix::enter();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("lock");
        let run = run_bounded(
            grace_flooder(GRACE_FLOODER, &marker),
            "flooder",
            Duration::from_millis(600),
            None,
        )
        .expect("a deadline keeps its prefix");
        // A deadline discards the exit status, so marker removal is the signal
        // that the child was reaped by the ask: only its SIGTERM trap removes it.
        assert!(
            !marker.exists(),
            "SIGTERM cleanup never ran: the child was wedged on a full pipe until SIGKILL"
        );
        assert!(
            String::from_utf8_lossy(&run.stderr).contains("cleaned"),
            "the cleanup line was not read: {}",
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(!run.success, "a deadline is never success");
        assert!(!run.cancelled);
        assert!(
            matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
            "{:?}",
            run.incomplete
        );
    }

    /// R2-02, cancel path: the same shape, reached through the observer.
    #[cfg(unix)]
    #[test]
    fn a_cancel_grace_keeps_draining_so_cleanup_after_sigterm_completes() {
        struct CancelOnceStarted(PathBuf);
        impl ProcessObserver for CancelOnceStarted {
            fn cancelled(&self) -> bool {
                self.0.exists()
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("lock");
        let mut observer = CancelOnceStarted(marker.clone());
        let mut cmd = grace_flooder(GRACE_FLOODER, &marker);
        let run = run_observed(
            &mut cmd,
            "flooder",
            Duration::from_secs(20),
            None,
            MAX_OUTPUT_BYTES,
            &mut observer,
        )
        .expect("a cancel returns the run");
        assert!(run.cancelled);
        assert!(!run.success);
        // Reaped by the ask: the trap exits 0, a SIGKILLed child reports -1.
        assert_eq!(run.status_code, 0, "the child was killed, not asked");
        assert!(
            !marker.exists(),
            "SIGTERM cleanup never ran: the child was wedged on a full pipe until SIGKILL"
        );
        assert!(
            String::from_utf8_lossy(&run.stderr).contains("cleaned"),
            "the cleanup line was not read: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }

    /// R2-02: a child that ignores the ask is still killed once the grace is
    /// spent, however much it writes meanwhile, and what it wrote while being
    /// asked is read. The ask is observed (the trap runs) only because the
    /// pipes are drained.
    #[cfg(unix)]
    #[test]
    fn a_child_that_ignores_sigterm_is_killed_at_the_end_of_a_drained_grace() {
        let _kept = KeepDeadlinePrefix::enter();
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("lock");
        let script = r#"
            trap 'head -c 80000 /dev/zero; echo term-seen >&2' TERM
            touch "$1"
            while :; do echo xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx; done
        "#;
        let started = Instant::now();
        let run = run_bounded(
            grace_flooder(script, &marker),
            "stubborn",
            Duration::from_millis(300),
            None,
        )
        .expect("a deadline keeps its prefix");
        assert!(
            started.elapsed()
                < crate::test_support::coverage_relaxed(
                    Duration::from_millis(300) + Duration::from_secs(2)
                ),
            "the grace must stay bounded: {:?}",
            started.elapsed()
        );
        assert!(matches!(run.incomplete, Some(Incomplete::Deadline { .. })));
        assert!(!run.success);
        assert!(
            String::from_utf8_lossy(&run.stderr).contains("term-seen"),
            "the trap could not run while the pipe was full: {}",
            String::from_utf8_lossy(&run.stderr)
        );
    }

    #[test]
    #[cfg(unix)]
    fn audit_observer_cancellation_preserves_progress_and_kills_child() {
        struct CancelOnOutput(Vec<u8>);
        impl ProcessObserver for CancelOnOutput {
            fn cancelled(&self) -> bool {
                !self.0.is_empty()
            }
            fn output(&mut self, _: OutputStream, bytes: &[u8]) {
                self.0.extend_from_slice(bytes);
            }
        }
        let mut observer = CancelOnOutput(Vec::new());
        let mut cmd = Command::new("sh");
        cmd.args(["-c", "printf progress; sleep 30"]);
        let started = Instant::now();
        let out = run_observed(
            &mut cmd,
            "cancel",
            Duration::from_secs(10),
            None,
            1024,
            &mut observer,
        )
        .unwrap();
        assert!(out.cancelled && !out.success);
        assert_eq!(out.stdout, b"progress");
        assert_eq!(observer.0, b"progress");
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)));
    }

    // ------------------------------------------------------------------
    // Adversarial batch-stream cases (CARD 16).
    // ------------------------------------------------------------------

    fn adv_answers(
        stream: &[u8],
        ids: &[&str],
        cut: Option<&str>,
    ) -> HashMap<String, Result<usize, CommandError>> {
        const OBJECT: BatchObject = BatchObject {
            kind: "commit",
            noun: "commit",
        };
        let mut answers = HashMap::new();
        read_batch_answers(stream, ids, OBJECT, &<[u8]>::len, cut, &mut answers);
        answers
    }

    /// A size that lies low ends the body mid-object, where no newline waits:
    /// that answer and every one after it are unread, never re-framed.
    #[test]
    fn a_size_that_lies_low_leaves_this_and_every_later_commit_unread() {
        let (a, b, c) = ("a".repeat(40), "b".repeat(40), "c".repeat(40));
        let stream = format!("{a} commit 2\nhi\n{b} commit 3\nabcdef\n{c} commit 1\nx\n");
        let got = adv_answers(stream.as_bytes(), &[&a, &b, &c], None);
        assert_eq!(got[&a], Ok(2));
        assert!(got[&b].is_err(), "{:?}", got[&b]);
        assert!(got[&c].is_err(), "{:?}", got[&c]);
    }

    /// A size that lies high swallows the next header: cut short, rest unread.
    #[test]
    fn a_size_that_lies_high_is_cut_short_not_misattributed() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let stream = format!("{a} commit 500\nhi\n{b} commit 1\nx\n");
        let got = adv_answers(stream.as_bytes(), &[&a, &b], None);
        assert!(got[&a].is_err() && got[&b].is_err(), "{got:?}");
    }

    /// A size too large for usize arithmetic.
    #[test]
    fn a_size_beyond_usize_is_an_error_not_a_panic() {
        let a = "a".repeat(40);
        for size in [
            "18446744073709551615",
            "99999999999999999999999",
            "-1",
            "1e3",
            "",
        ] {
            let stream = format!("{a} commit {size}\nhi\n");
            let got = adv_answers(stream.as_bytes(), &[&a], None);
            assert!(got[&a].is_err(), "{size:?}: {got:?}");
        }
    }

    /// The stream ends in the middle of a header line, then in the middle of
    /// the very first byte: every id is answered, none as a value.
    #[test]
    fn a_stream_cut_inside_a_header_or_empty_leaves_all_unread() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let half = format!("{a} commit 2\nhi\n{b} comm");
        let got = adv_answers(half.as_bytes(), &[&a, &b], None);
        assert_eq!(got[&a], Ok(2));
        assert!(got[&b].is_err());
        let none = adv_answers(b"", &[&a, &b], None);
        assert!(none[&a].is_err() && none[&b].is_err());
    }

    /// `missing` and `ambiguous` carry no body, so the next answer stays in
    /// step; a stray extra header field is an error for that id only.
    #[test]
    fn missing_and_ambiguous_answers_keep_the_following_answers_in_step() {
        let (a, b, c, d) = (
            "a".repeat(40),
            "b".repeat(40),
            "c".repeat(40),
            "d".repeat(40),
        );
        let stream =
            format!("{a} missing\n{b} ambiguous\n{c} commit 3\nabc\n{d} commit 1 extra\nx\n");
        let got = adv_answers(stream.as_bytes(), &[&a, &b, &c, &d], None);
        assert!(got[&a].is_err());
        assert!(got[&b].is_err());
        assert_eq!(got[&c], Ok(3));
        assert!(got[&d].is_err());
    }

    /// A commit message that is itself a perfectly formed batch answer for the
    /// next id, with an honest size from git, must stay inside its own body.
    #[test]
    fn a_body_that_forges_the_next_answer_stays_inside_its_own_size() {
        let (a, b) = ("a".repeat(40), "b".repeat(40));
        let forged = format!("{b} commit 5\nFORGE\n");
        let stream = format!(
            "{a} commit {}\n{forged}\n{b} commit 4\nreal\n",
            forged.len()
        );
        let got = adv_answers(stream.as_bytes(), &[&a, &b], None);
        assert_eq!(got[&a], Ok(forged.len()));
        assert_eq!(
            got[&b],
            Ok(4),
            "b is the answer after a's own body, not the forgery"
        );
    }

    /// 100,005 ids in one call: the first 100,000 are asked, the rest are
    /// errors, and the call returns without stalling on a full pipe.
    #[test]
    fn an_oversized_id_list_is_capped_and_does_not_stall_the_pipe() {
        let dir = tempfile::TempDir::new().unwrap();
        crate::test_support::git_in(dir.path(), &["init", "-b", "main"]);
        let oids: Vec<String> = (0..100_005u64).map(|n| format!("{n:040x}")).collect();
        let started = std::time::Instant::now();
        let got = read_objects_batch(
            dir.path(),
            &oids,
            BatchObject {
                kind: "commit",
                noun: "commit",
            },
            64 * 1024 * 1024,
            &<[u8]>::len,
        );
        assert!(
            started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(60)),
            "{:?}",
            started.elapsed()
        );
        assert_eq!(got.len(), oids.len(), "every id gets an answer");
        assert!(got.values().all(Result::is_err), "nothing here is a commit");
        let capped = got
            .values()
            .filter(|r| r.as_ref().unwrap_err().contains("asked for at once"))
            .count();
        assert_eq!(capped, 5);
    }
}

/// The cross-process budget, the command scope and the post-action credit.
#[cfg(test)]
mod admission_scope_tests {
    use super::*;
    #[cfg(unix)]
    use crate::procguard::LockedSpawn;

    fn leak(gate: SpawnGate) -> &'static SpawnGate {
        Box::leak(Box::new(gate))
    }

    /// A gate whose bucket cannot refill during the test. The rate is real
    /// time, so on a loaded machine the second between two spawns that a test
    /// calls "budget spent" arrives, a token lands, and the spawn it expected
    /// to be refused is admitted. Putting the bucket's clock in the future
    /// makes elapsed time zero for as long as the test runs.
    fn frozen(gate: SpawnGate) -> &'static SpawnGate {
        let gate = leak(gate);
        gate.state.lock().unwrap().tokens_at = Some(Instant::now() + Duration::from_secs(86_400));
        gate
    }

    /// Keeps `repo`'s post-action credit open for the test: its window is real
    /// time too, and thirty-two real process spawns do not fit in three
    /// seconds on a busy machine. The window's own expiry is pinned by
    /// `a_credit_expires_with_its_window`, against explicit instants.
    #[cfg(unix)]
    fn hold_credit_open(repo: &Path) {
        let mut credits = post_action_credits().lock().unwrap();
        credits.get_mut(repo).expect("a credit was granted").until =
            Instant::now() + Duration::from_secs(86_400);
    }

    fn shared_gate(path: &Path, burst: u32, rate: u32) -> &'static SpawnGate {
        let gate = SpawnGate::with_rate(16, burst, rate).sharing(Ok(path.to_path_buf()));
        assert!(gate.shared.is_some(), "{:?}", gate.shared_note);
        leak(gate)
    }

    fn admits(gate: &SpawnGate) -> bool {
        gate.acquire(Instant::now() + Duration::from_millis(20))
            .is_some()
    }

    #[test]
    fn two_processes_sharing_one_budget_obey_one_combined_burst() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        // Two gates opening one record are two open-file descriptions, which
        // is what two processes hold.
        let a = shared_gate(&path, 6, 1);
        let b = shared_gate(&path, 6, 1);
        let started = Instant::now();
        let admitted = (0..20)
            .filter(|i| admits(if i % 2 == 0 { a } else { b }))
            .count();
        // One refill token per elapsed second may land meanwhile.
        let refill = started.elapsed().as_secs() as usize + 1;
        assert!(
            (6..=6 + refill).contains(&admitted),
            "admitted {admitted}: one burst between both, not one each"
        );
    }

    #[test]
    fn background_is_shed_at_the_shared_reserve_whoever_spent_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 8, 1);
        let b = shared_gate(&path, 8, 1);
        for _ in 0..6 {
            assert!(admits(a));
        }
        // B has spent nothing itself, but together the two are at the
        // reserve (8 / 4 = 2), so B's background work is shed at once.
        let shed = with_admission(Admission::Background, || {
            b.acquire_until(Instant::now() + Duration::from_secs(5), &|| false)
                .map(|_| ())
        });
        assert_eq!(shed.err(), Some(Refusal::Shed));
        assert!(admits(b), "a reactive read still has the reserve");
    }

    /// B has every process-bucket token, and the shared record is spent. When
    /// B cannot rewrite the record the denial is still a denial: no
    /// non-interactive spawn starts and it is not counted as a fallback.
    /// A user action never asks the record, so it still starts.
    #[test]
    fn a_denial_the_record_cannot_persist_does_not_admit_a_background_spawn() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 8, 1);
        let b = shared_gate(&path, 8, 1);
        while admits(a) {}
        b.shared.as_ref().unwrap().fail_writes(true);
        assert!(
            !admits(b),
            "a reactive spawn rode a failed write past the budget"
        );
        let shed = with_admission(Admission::Background, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(shed.err(), Some(Refusal::Shed));
        assert_eq!(b.state.lock().unwrap().shared_fallbacks, 0);
        // The report counts those denials on their own line, apart from the
        // ordinary denial A met when it spent the record and from fallbacks.
        let (fallbacks, unrecorded) = {
            let state = b.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert!(unrecorded >= 2, "the reactive and the background denial");
        let report = b.sharing_report(fallbacks, unrecorded);
        assert!(
            report.ends_with(&format!("unrecorded_denials={unrecorded} fallbacks=0")),
            "{report}"
        );
        let ordinary = {
            let state = a.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert_eq!(ordinary, (0, 0), "a recorded denial is neither");
        let clicked = with_admission(Admission::Interactive, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(clicked, Ok(()), "a user action does not ask the record");
    }

    /// Spawns `gate` admits in `class` before it refuses, its own bucket
    /// frozen so only the budgets decide.
    fn drain(gate: &SpawnGate, class: Admission) -> usize {
        // The deadline is checked before the attempt. Background runs at
        // utility QoS, and a stall longer than the deadline is counted as an
        // empty bucket. An empty background bucket returns `Refusal::Shed`
        // without waiting, so the longer cap does not add grants. Reactive
        // stays on the short deadline: it waits out an empty bucket, and a
        // long one would pad every measurement with that wait.
        let budget = if class == Admission::Background {
            Duration::from_secs(1)
        } else {
            Duration::from_millis(20)
        };
        let mut admitted = 0;
        while with_admission(class, || gate.acquire(Instant::now() + budget).is_some()) {
            admitted += 1;
        }
        admitted
    }

    /// CHARACTERIZATION, awaiting the owner's ruling (ft-d41b8491, part 2): a
    /// grant whose record write fails is the documented fail-open, so when
    /// writes fail on every take the record never decrements and the shared
    /// cap is gone: each of N processes is granted its own whole bucket. This
    /// pins the aggregate as it is, with the healthy record beside it, so a
    /// ruling that bounds it (a fixed floor after repeated write failures)
    /// has to change this test on purpose.
    #[test]
    fn writes_failing_on_every_take_grant_each_process_its_own_bucket() {
        let (burst, rate) = production_spawn_rate();
        let reserve = (burst / 4) as usize;
        let burst = burst as usize;
        let mut rows = Vec::new();
        for processes in [1usize, 2, 4, 8] {
            let mut aggregate = [0usize; 4];
            for (slot, (writes_fail, class)) in [
                (false, Admission::Reactive),
                (true, Admission::Reactive),
                (false, Admission::Background),
                (true, Admission::Background),
            ]
            .into_iter()
            .enumerate()
            {
                let dir = tempfile::tempdir().unwrap();
                let path = dir.path().join("budget");
                let gates: Vec<_> = (0..processes)
                    .map(|_| {
                        let gate =
                            SpawnGate::with_rate(16, burst as u32, rate).sharing(Ok(path.clone()));
                        let gate = frozen(gate);
                        gate.shared.as_ref().unwrap().fail_writes(writes_fail);
                        gate
                    })
                    .collect();
                let started = Instant::now();
                aggregate[slot] = gates.iter().map(|gate| drain(gate, class)).sum();
                // The shared record refills in real time at `rate`.
                let refill =
                    (started.elapsed().as_secs_f64() * f64::from(rate)).ceil() as usize + 1;
                let (own, shared) = match class {
                    Admission::Background => (burst - reserve, burst - reserve),
                    _ => (burst, burst),
                };
                if writes_fail {
                    assert_eq!(
                        aggregate[slot],
                        processes * own,
                        "{processes} processes, {class:?}: each its own bucket"
                    );
                } else {
                    assert!(
                        (shared..=shared + refill).contains(&aggregate[slot]),
                        "{processes} processes, {class:?}: {} for one shared bucket",
                        aggregate[slot]
                    );
                }
            }
            rows.push(format!(
                "N={processes}: reactive healthy {} / writes failing {}; background healthy {} / writes failing {}",
                aggregate[0], aggregate[1], aggregate[2], aggregate[3]
            ));
        }
        println!("{}", rows.join("\n"));
    }

    /// The two counters are separate, add exactly under concurrent deciders of
    /// one gate, and saturate rather than wrap; the reason (which names the
    /// path) is only logged, never part of the report's counts.
    #[test]
    fn unrecorded_denials_and_fallbacks_count_apart_exactly_and_saturate() {
        let dir = tempfile::tempdir().unwrap();
        let gate = shared_gate(&dir.path().join("budget"), 8, 1);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    for i in 0..500 {
                        let taken = if i % 5 == 0 {
                            shared_budget::Take::Unavailable("held".into())
                        } else {
                            shared_budget::Take::DeniedUnrecorded("full".into())
                        };
                        let mut state = gate.state.lock().unwrap();
                        assert_eq!(gate.settle_shared(&mut state, taken), i % 5 == 0);
                    }
                });
            }
        });
        let (fallbacks, unrecorded) = {
            let state = gate.state.lock().unwrap();
            (state.shared_fallbacks, state.shared_unrecorded_denials)
        };
        assert_eq!((fallbacks, unrecorded), (200, 800));

        let mut state = gate.state.lock().unwrap();
        state.shared_unrecorded_denials = u64::MAX;
        state.shared_fallbacks = u64::MAX;
        assert!(!gate.settle_shared(
            &mut state,
            shared_budget::Take::DeniedUnrecorded("x".into())
        ));
        assert!(gate.settle_shared(&mut state, shared_budget::Take::Unavailable("x".into())));
        assert_eq!(
            (state.shared_fallbacks, state.shared_unrecorded_denials),
            (u64::MAX, u64::MAX)
        );
        let report = gate.sharing_report(state.shared_fallbacks, state.shared_unrecorded_denials);
        assert!(
            report.ends_with(&format!(
                "unrecorded_denials={} fallbacks={}",
                u64::MAX,
                u64::MAX
            )),
            "{report}"
        );
    }

    #[test]
    fn a_user_action_neither_waits_for_nor_spends_the_shared_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let a = shared_gate(&path, 3, 1);
        let b = shared_gate(&path, 3, 1);
        while admits(a) {}
        let clicked = with_admission(Admission::Interactive, || {
            b.acquire_until(Instant::now() + Duration::from_millis(50), &|| false)
                .map(|_| ())
        });
        assert_eq!(clicked, Ok(()));
        let shared = b.shared.as_ref().unwrap();
        assert_eq!(
            shared.take(0, 3, 1),
            shared_budget::Take::Denied,
            "the click took no shared token"
        );
    }

    /// The shared record's file lock can sleep-poll for a few milliseconds and
    /// a denial writes the file. With the gate mutex held across that, every
    /// other acquire, a user's click included, queued behind a background
    /// re-poll.
    #[test]
    fn a_user_action_does_not_queue_behind_the_shared_budget_io() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 100_000, 100_000);
        // Another process pinning the record: every shared take now waits out
        // the whole lock patience before falling back.
        let holder = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        holder.lock().unwrap();
        let stop = std::sync::Arc::new(AtomicBool::new(false));
        let pollers: Vec<_> = (0..3)
            .map(|_| {
                let stop = std::sync::Arc::clone(&stop);
                std::thread::spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        let _ = gate.acquire(Instant::now() + Duration::from_millis(200));
                    }
                })
            })
            .collect();
        std::thread::sleep(Duration::from_millis(30));
        let mut fastest = Duration::MAX;
        for _ in 0..20 {
            let started = Instant::now();
            let clicked = with_admission(Admission::Interactive, || {
                gate.acquire_until(Instant::now() + Duration::from_millis(500), &|| false)
                    .map(|_| ())
            });
            fastest = fastest.min(started.elapsed());
            assert_eq!(clicked, Ok(()));
            std::thread::sleep(Duration::from_millis(3));
        }
        stop.store(true, Ordering::Relaxed);
        for poller in pollers {
            poller.join().unwrap();
        }
        holder.unlock().unwrap();
        assert!(
            fastest < Duration::from_millis(5),
            "every user action waited behind shared-budget IO: fastest was {fastest:?}"
        );
    }

    #[test]
    fn an_unusable_shared_record_leaves_the_gate_on_its_own_budget_and_says_so() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the record should be.
        let gate = leak(SpawnGate::with_rate(16, 4, 1).sharing(Ok(dir.path().to_path_buf())));
        assert!(gate.shared.is_none());
        let report = gate.sharing_report(0, 0);
        assert!(report.starts_with("budget=per-process ("), "{report}");
        let admitted = (0..8).filter(|_| admits(gate)).count();
        assert_eq!(admitted, 4, "the process bucket still holds");

        let unpathed = SpawnGate::with_rate(16, 4, 1).sharing(Err("no home".into()));
        assert_eq!(
            unpathed.sharing_report(0, 0),
            "budget=per-process (no home)"
        );
        // The unlimited test gate has nothing to share, and says that.
        let unlimited = SpawnGate::new(4).sharing(Ok(dir.path().join("x")));
        assert!(unlimited.shared.is_none());
        assert!(!dir.path().join("x").exists(), "no record created for it");
    }

    #[test]
    fn a_held_shared_lock_falls_back_to_the_process_budget_and_is_counted() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 4, 1);
        let holder = std::fs::File::options()
            .read(true)
            .write(true)
            .open(&path)
            .unwrap();
        holder.lock().unwrap();
        assert!(admits(gate), "a stopped holder must not stop this process");
        let fallbacks = gate.state.lock().unwrap().shared_fallbacks;
        assert_eq!(fallbacks, 1);
        assert!(gate.sharing_report(fallbacks, 0).ends_with("fallbacks=1"));
        holder.unlock().unwrap();
    }

    #[test]
    fn the_report_names_where_the_budget_lives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("budget");
        let gate = shared_gate(&path, 4, 1);
        let report = gate.sharing_report(0, 0);
        assert!(report.starts_with("budget=shared("), "{report}");
        assert!(report.contains(&path.display().to_string()), "{report}");
    }

    fn deferral() -> CommandError {
        refusal_message(
            "git status",
            Refusal::Refused {
                waited: Duration::from_millis(2_013),
            },
        )
    }

    #[test]
    fn a_read_commands_deferral_stays_retryable() {
        let out: Result<(), CommandError> = run_command_scope(|| Err(deferral()));
        assert!(is_deferred_under_load(out.unwrap_err().message()));
    }

    #[test]
    fn a_user_actions_deferral_is_never_handed_out_as_retryable() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        let out: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(&repo);
            Err(deferral())
        });
        let message = out.unwrap_err();
        assert!(
            !is_deferred_under_load(message.message()),
            "{message}",
            message = message.message()
        );
        assert!(
            message.contains("deferred under load"),
            "the cause stays: {message}",
            message = message.message()
        );
        assert!(
            message.contains("2.013"),
            "{message}",
            message = message.message()
        );
        // Other failures pass through untouched.
        let plain: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(&repo);
            Err("fatal: bad revision".into())
        });
        assert_eq!(plain.unwrap_err(), "fatal: bad revision");
    }

    #[test]
    fn a_command_scope_restores_the_class_and_repo_even_when_it_panics() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_str().unwrap().to_string();
        assert_eq!(current_admission(), Admission::Reactive);
        let caught = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _: Result<(), CommandError> = run_command_scope(|| {
                mark_user_action(&repo);
                panic!("expected");
            });
        }));
        assert!(caught.is_err());
        assert_eq!(current_admission(), Admission::Reactive);
        assert!(MARKED_REPO.with(|slot| slot.borrow().is_none()));
        // A panicking action grants nothing.
        let canonical = dir.path().canonicalize().unwrap();
        assert!(!take_post_action_credit(&canonical, Instant::now()));
    }

    #[cfg(unix)]
    fn reactive_spawn_in(gate: &'static SpawnGate, cwd: &Path) -> Result<BoundedRun, CommandError> {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "true"]).current_dir(cwd);
        run_with_gate(
            &mut cmd,
            "credit-probe",
            Duration::from_millis(200),
            None,
            1024,
            &mut (),
            gate,
        )
    }

    #[cfg(unix)]
    #[test]
    fn the_refresh_after_a_user_action_is_admitted_within_its_credit_and_no_further() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let other = tempfile::tempdir().unwrap();
        let other = other.path().canonicalize().unwrap();
        // One token, and no refill at all inside this test.
        let gate = frozen(SpawnGate::with_rate(16, 1, 1));
        reactive_spawn_in(gate, &repo).expect("the one token");
        let starved = reactive_spawn_in(gate, &repo).unwrap_err();
        assert!(
            is_deferred_under_load(starved.message()) || is_slot_wait_timeout(starved.message()),
            "{starved}",
            starved = starved.message()
        );

        let acted: Result<(), CommandError> = run_command_scope(|| {
            mark_user_action(repo.to_str().unwrap());
            Ok(())
        });
        acted.unwrap();
        // The credit is per repository, and a read command never earns one.
        let read: Result<(), CommandError> = run_command_scope(|| Ok(()));
        read.unwrap();
        assert!(reactive_spawn_in(gate, &other).is_err());
        hold_credit_open(&repo);
        for i in 0..POST_ACTION_CREDIT_SPAWNS {
            reactive_spawn_in(gate, &repo)
                .unwrap_or_else(|e| panic!("credited spawn {i} refused: {e}", e = e.message()));
        }
        // Spent, and the bucket cannot refill: nothing more is admitted.
        let extra = (0..6)
            .take_while(|_| reactive_spawn_in(gate, &repo).is_ok())
            .count();
        assert_eq!(
            extra, 0,
            "{extra} spawns past the {POST_ACTION_CREDIT_SPAWNS}-spawn credit"
        );
    }

    /// Regression: a tab switch is a user action whose hydrate is all reads,
    /// so it ran Reactive and was deferred behind refresh traffic for tabs
    /// nobody was looking at. Activation now earns the post-action credit.
    #[cfg(unix)]
    #[test]
    fn a_tab_activation_admits_its_hydrate_when_the_budget_is_spent() {
        let dir = crate::test_support::git_repo();
        let repo = dir.path().canonicalize().unwrap();
        let gate = frozen(SpawnGate::with_rate(16, 1, 1));
        reactive_spawn_in(gate, &repo).expect("the one token");
        assert!(reactive_spawn_in(gate, &repo).is_err(), "budget spent");

        note_tab_activated(repo.to_str().unwrap()).expect("activation");
        hold_credit_open(&repo);
        for i in 0..POST_ACTION_CREDIT_SPAWNS {
            reactive_spawn_in(gate, &repo)
                .unwrap_or_else(|e| panic!("activation read {i} refused: {e}", e = e.message()));
        }
    }

    /// What the two credit tests rely on: a frozen bucket takes no token from
    /// the passing of time, however long a loaded machine makes a test, while
    /// an ordinary one does.
    #[test]
    fn a_frozen_bucket_does_not_refill_and_an_ordinary_one_does() {
        let later = Instant::now() + Duration::from_secs(30);
        for (gate, refilled) in [
            (frozen(SpawnGate::with_rate(16, 4, 1)), false),
            (leak(SpawnGate::with_rate(16, 4, 1)), true),
        ] {
            let mut state = gate.state.lock().unwrap();
            state.tokens = 0;
            if refilled {
                // An ordinary gate starts its clock at its first look.
                state.tokens_at = Some(Instant::now());
            }
            gate.refill(&mut state, later);
            assert_eq!(state.tokens > 0, refilled, "tokens {}", state.tokens);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_tab_activation_earns_nothing_for_an_untrusted_or_missing_repository() {
        let dir = tempfile::tempdir().unwrap();
        let untrusted = dir.path().canonicalize().unwrap();
        // Not `test_support::git_in`: its `init` approves the fixture.
        let status = std::process::Command::new("git")
            .args(["init", "--quiet"])
            .current_dir(&untrusted)
            .status_locked()
            .expect("git init");
        assert!(status.success());
        let refused = note_tab_activated(untrusted.to_str().unwrap()).unwrap_err();
        assert!(
            crate::repository_trust::refused(refused.message()),
            "{refused}",
            refused = refused.message()
        );
        assert!(!take_post_action_credit(&untrusted, Instant::now()));

        let missing = untrusted.join("gone");
        assert!(note_tab_activated(missing.to_str().unwrap()).is_err());
        assert!(!take_post_action_credit(&missing, Instant::now()));
        assert!(note_tab_activated("relative/path").is_err());
        assert!(note_tab_activated("").is_err());
    }

    #[test]
    fn a_credit_expires_with_its_window() {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let at = Instant::now();
        grant_post_action_credit(&repo, at);
        assert!(take_post_action_credit(
            &repo,
            at + Duration::from_millis(10)
        ));
        assert!(!take_post_action_credit(
            &repo,
            at + POST_ACTION_CREDIT_WINDOW + Duration::from_millis(1)
        ));
        // Expired entries are dropped, so the map cannot grow past its bound.
        for i in 0..(POST_ACTION_CREDIT_REPOS + 8) {
            grant_post_action_credit(&repo.join(i.to_string()), at);
        }
        let held = post_action_credits().lock().unwrap().len();
        assert!(held <= POST_ACTION_CREDIT_REPOS, "{held}");
    }

    #[cfg(unix)]
    #[test]
    fn a_swallowed_failure_can_still_be_reported() {
        let gate = leak(SpawnGate::new(2));
        let dir = tempfile::tempdir().unwrap();
        let (failures, message) = with_forced_spawn_failure(|| {
            let before = process_failures();
            let _ = reactive_spawn_in(gate, dir.path());
            (process_failures() - before, last_process_failure())
        });
        assert_eq!(failures, 1);
        assert!(message.is_some_and(|message| message.contains("forced by test")));
    }
}

/// The admission class decides which children and threads are starved.
#[cfg(all(test, target_os = "macos"))]
mod qos_band_tests {
    use super::*;

    /// Polls of 50 ms a probe child waits to see itself in the band. The child
    /// enters it before it execs, so what is waited for is `ps` showing it
    /// (a new image can read a transient higher priority first); the polls are
    /// also a hang backstop for a host so loaded that `ps` itself is starved,
    /// and a probe that gives up first proves nothing.
    const POLLS_TO_BE_DEMOTED: u32 = 1200;

    /// A child that reports its own scheduler priority once it reads as being
    /// in the band, or after `waits` polls of 50 ms when it never does. `ps`
    /// reports the background band as 4 and a default process far above 10.
    fn priority_probe(waits: u32) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(format!(
            "i=0; p=; while [ $i -lt {waits} ]; do \
             p=$(/bin/ps -o pri= -p $$ | tr -d ' '); \
             [ \"$p\" -lt 10 ] && break; sleep 0.05; i=$((i+1)); done; \
             /bin/ps -o pri= -p $$ | tr -d ' '"
        ));
        cmd
    }

    /// Runs a probe child under the current class and returns its priority,
    /// the demotions that succeeded and the ones the OS refused meanwhile.
    fn priority_and_outcomes(waits: u32) -> (i32, u64, u64) {
        let (entered, failed) = band_outcomes();
        let priority = priority_of_child(waits);
        let (entered_after, failed_after) = band_outcomes();
        (priority, entered_after - entered, failed_after - failed)
    }

    fn priority_of_child(waits: u32) -> i32 {
        // A background spawn is shed when the shared budget is low; waiting
        // for it makes the test about the band, not about the budget.
        let run = with_background_budget_wait(true, || {
            run_bounded(
                priority_probe(waits),
                "priority-probe",
                crate::test_support::coverage_relaxed(Duration::from_secs(120)),
                None,
            )
        })
        .expect("probe ran");
        String::from_utf8_lossy(&run.stdout)
            .trim()
            .parse()
            .expect("priority")
    }

    #[test]
    fn a_background_class_child_is_in_the_background_band() {
        let (priority, entered, failed) = with_admission(Admission::Background, || {
            priority_and_outcomes(POLLS_TO_BE_DEMOTED)
        });
        assert_eq!(failed, 0, "the OS refused a demotion");
        assert!(entered >= 1, "no demotion succeeded");
        assert!(priority < 10, "background child ran at priority {priority}");
    }

    /// The mis-classed write must never be starved: a user action and a plain
    /// read both stay at default priority. The probe waits long enough that a
    /// late demotion would have shown.
    #[test]
    fn a_user_action_child_is_not_in_the_background_band() {
        for class in [Admission::Interactive, Admission::Reactive] {
            let priority = with_admission(class, || priority_of_child(12));
            assert!(
                priority >= 10,
                "{} child ran at priority {priority}",
                class.name()
            );
        }
    }

    #[test]
    fn the_background_band_scope_demotes_a_child_of_any_class() {
        let (priority, entered, failed) = with_admission(Admission::Reactive, || {
            with_background_band(|| priority_and_outcomes(POLLS_TO_BE_DEMOTED))
        });
        assert_eq!(failed, 0, "the OS refused a demotion");
        assert!(entered >= 1, "no demotion succeeded");
        assert!(priority < 10, "scoped child ran at priority {priority}");
        let after = with_admission(Admission::Reactive, || priority_of_child(12));
        assert!(after >= 10, "the scope leaked: priority {after}");
    }

    #[test]
    fn background_scopes_run_the_thread_at_utility_and_give_it_back() {
        use crate::pools::{current_thread_qos, QOS_UTILITY};
        std::thread::spawn(|| {
            let class = || current_thread_qos().expect("qos").0;
            assert_ne!(class(), QOS_UTILITY);
            with_background_processes(|| {
                assert_eq!(class(), QOS_UTILITY);
                // Nested background scope: unchanged, and its exit must not
                // restore early.
                with_background_processes(|| assert_eq!(class(), QOS_UTILITY));
                assert_eq!(class(), QOS_UTILITY);
            });
            assert_ne!(class(), QOS_UTILITY);
            with_admission(Admission::Interactive, || {
                assert_ne!(class(), QOS_UTILITY);
            });
            let original = class();
            with_background_processes(|| {
                with_admission(Admission::Interactive, || assert_eq!(class(), original));
                assert_eq!(class(), QOS_UTILITY);
            });
            assert_eq!(class(), original);
        })
        .join()
        .expect("qos thread");
    }
}

#[cfg(test)]
#[path = "scrubbed_env_adversarial_tests.rs"]
mod scrubbed_env_adversarial_tests;
