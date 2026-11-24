pub(crate) use super::process::read_cancel;
#[cfg(windows)]
use super::process::simplified_windows_path;
#[cfg(all(test, target_os = "macos"))]
use super::process::XCRUN_GIT_SHIM;
#[cfg(test)]
use super::process::{
    build_capture_command_in, is_deferred_under_load, is_injected_git_env, last_process_failure,
    process_failures, rev_parse_launches_from, PathSource, SLOT_WAIT_SUFFIX,
};
#[cfg(target_os = "macos")]
use super::process::{developer_dirs, past_xcrun_git_shim, REAL_GIT_DIRS};
use super::process::{
    extended_child_path, find_in_dirs, grant_post_action_credit_now, note_process_failure,
    pin_git_environment, run_bounded, run_bounded_capped, run_observed, run_read_shared,
    text_from_bytes, with_spawn_source, BoundedRun, Incomplete, KeepDeadlinePrefix,
    ProcessObserver, DEFAULT_TIMEOUT, MAX_OUTPUT_BYTES, NETWORK_TIMEOUT, TIMEOUT_MARKER,
};
#[cfg(all(test, unix))]
use super::process::{
    frozen_gate as frozen, hold_credit_open, spawn_labels_during, spawn_log,
    take_post_action_credit, SpawnGate, POST_ACTION_CREDIT_SPAWNS,
};
use crate::diff::{Expect, WriteFailure};
use crate::ipc_error::CommandError;
use serde::{Deserialize, Serialize};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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
    FORCE_IF_INCLUDES, HOOK_LIST, MERGE_TREE_WRITE_TREE, SINCE_AS_FILTER,
};

#[cfg(test)]
#[path = "git_cli/run_errors_adversarial_tests.rs"]
mod run_errors_adversarial_tests;

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

/// What the spawn-gate report calls the `rev-parse` launches [`resolve_repo`]
/// makes.
pub(crate) const RESOLVE_REPO_SOURCE: &str = "git_cli.resolve_repo";

fn rev_parse_is_bare(path: &Path) -> bool {
    match with_spawn_source(RESOLVE_REPO_SOURCE, || {
        git_text(path, &["rev-parse", "--is-bare-repository"])
    }) {
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

/// Config every git child is run with, ahead of its subcommand, so a
/// developer's own settings cannot change what the parsers read:
/// `core.quotepath` keeps non-ASCII paths as raw bytes; `log.showSignature`
/// prints a "No signature" line (and runs gpg) in front of every commit id;
/// the colour settings put ANSI escapes in diffs (the specific keys outrank
/// `color.ui`, so they are pinned too); `diff.noprefix` and
/// `diff.mnemonicPrefix` change the `a/` `b/` prefixes the diff parser keys on.
pub(crate) const PINNED_GIT_CONFIG: &[&str] = &[
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
    // Apple Git's `fsmonitor--daemon` lacks `pthread_cond_timedwait` and hangs
    // indefinitely in `_pthread_cond_wait` when FSEvents drops or delays cookie
    // notifications under memory pressure; any git subcommand (`status`, `diff`,
    // `ls-files`, `stash`, etc.) attaching to `.git/fsmonitor--daemon.ipc` then
    // stalls for the full 90s timeout, and fsmonitor cookie files defeat macOS
    // `PollSkip`. Pinning `core.fsmonitor=false` bypasses the IPC socket on all
    // spawned git commands.
    "core.fsmonitor=false",
];

/// The one porcelain status read: NUL-separated records, raw UTF-8 paths, and
/// every untracked file listed individually (`-u`) rather than collapsed into
/// its directory. The engine status reader and worktree scans both read
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

/// The subcommands that run a `diff.<driver>.textconv` program on repository
/// bytes: the patch printers plus `blame`. The converter's text would come back
/// as the "patch" (which `git apply` refuses) or as blamed lines. `blame` takes
/// `--no-textconv` but rejects `--no-ext-diff`, so it is guarded separately.
/// The same rule as `dc_proc::guarded_args` (`vendored/dc-proc`), which this
/// crate does not depend on; a parity test reads that source.
const TEXTCONV_SUBCOMMANDS: &[&str] = &["diff", "show", "log", "blame"];

/// The patch subcommands that print a signature header (and run the verifier
/// to do it) when `log.showSignature` or `--show-signature` asks. `--no-show-signature`
/// rides every one of them from here, so no read has to remember it; `diff` is
/// left out because it rejects the flag. It turns off that header only: it does
/// not touch the `%G*` placeholders, which always verify, and those are kept
/// out of every bulk format string by a source-scan test.
const SIGNATURE_PRINTING_SUBCOMMANDS: &[&str] = &["show", "log"];

#[cfg(test)]
thread_local! {
    static TEST_GIT_PATH: std::cell::RefCell<Option<std::ffi::OsString>> =
        const { std::cell::RefCell::new(None) };
}

/// Current thread-local test `PATH` override for helpers that hop threads.
#[cfg(test)]
pub(crate) fn current_test_git_path() -> Option<std::ffi::OsString> {
    TEST_GIT_PATH.with(|slot| slot.borrow().clone())
}

/// Overrides `PATH` for `git_command` on the current thread only, restoring on drop.
#[cfg(test)]
pub(crate) fn with_test_git_path<T>(path: &std::ffi::OsStr, f: impl FnOnce() -> T) -> T {
    struct Reset(Option<std::ffi::OsString>);
    impl Drop for Reset {
        fn drop(&mut self) {
            TEST_GIT_PATH.with(|slot| *slot.borrow_mut() = self.0.take());
        }
    }
    let prev = TEST_GIT_PATH.with(|slot| slot.borrow_mut().replace(path.to_os_string()));
    let _reset = Reset(prev);
    f()
}

/// Reads `PATH`/`HOME` from the process and defers to
/// [`git_command_with_env`], mirroring the [`capture_command`] /
/// [`capture_command_with_env`] split so the environment stays injectable for
/// tests instead of being read from under them.
fn git_command(repo: Option<&Path>, args: &[&str]) -> Command {
    #[cfg(test)]
    let path_var = TEST_GIT_PATH
        .with(|slot| slot.borrow().clone())
        .or_else(|| std::env::var_os("PATH"));
    #[cfg(not(test))]
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
    let is_config_cmd = subcommand_index(args).is_some_and(|at| args[at] == "config");
    for pin in PINNED_GIT_CONFIG {
        if is_config_cmd && *pin == "core.fsmonitor=false" {
            continue;
        }
        cmd.args(["-c", pin]);
    }
    match subcommand_index(args) {
        Some(at) if TEXTCONV_SUBCOMMANDS.contains(&args[at]) => {
            cmd.args(&args[..=at]);
            if PATCH_SUBCOMMANDS.contains(&args[at]) {
                cmd.arg("--no-ext-diff");
            }
            cmd.arg("--no-textconv");
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
    Ok(text_from_bytes(bytes))
}

/// Like [`git_text`], but an over-cap stream is data rather than an error:
/// returns what arrived plus the truncation flag. For callers whose input
/// tolerates a prefix (coverage family detection), where failing the whole
/// scan would be worse than reading part of the listing.
pub fn git_text_partial(repo: &Path, args: &[&str]) -> Result<(String, bool), CommandError> {
    let (bytes, incomplete) = git_run(Some(repo), args, DEFAULT_TIMEOUT, None)?;
    Ok((text_from_bytes(bytes), incomplete.is_some()))
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
    Ok((text_from_bytes(bytes), incomplete))
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

/// [`git_captured_with_stdin`] under a caller-owned deadline, for a question
/// whose answer is worth less than the default ninety seconds (the watcher's
/// ignore queries).
pub(crate) fn git_captured_with_stdin_timeout(
    repo: &Path,
    args: &[&str],
    stdin_bytes: &[u8],
    timeout: Duration,
) -> Result<BoundedRun, CommandError> {
    crate::repository_trust::require(repo)?;
    let label = format!("git {}", subcommand(args));
    run_bounded_capped(
        git_command(Some(repo), args),
        &label,
        timeout,
        Some(stdin_bytes),
        MAX_OUTPUT_BYTES,
    )
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
    Ok(text_from_bytes(bytes))
}

/// Runs network-bound git plumbing in `repo` under [`NETWORK_TIMEOUT`].
pub fn git_text_network(repo: &Path, args: &[&str]) -> Result<String, CommandError> {
    git_text_with_timeout(repo, args, NETWORK_TIMEOUT)
}

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
    grant_post_action_credit_now(&repo);
    Ok(())
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
    let _restore = std::mem::DropGuard::new(NO_LAZY_FETCH.replace(true), |prev| {
        NO_LAZY_FETCH.set(prev);
    });
    body()
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
    Ok(text_from_bytes(bytes))
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
    Ok((text_from_bytes(bytes), incomplete))
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
    // The request this thread serves, when its caller can cancel it (see
    // [`read_cancel`]). A command that runs hooks is a write and has its own
    // cancel, so it never takes this one.
    let cancel = read_cancel::current().filter(|_| hooked.is_none());
    loop {
        // A cancelled request starts nothing more: a reader that falls back to
        // a second command when the first fails must not spawn it for a caller
        // who has gone.
        if cancel
            .as_ref()
            .is_some_and(read_cancel::ReadCancel::is_cancelled)
        {
            return Err(read_cancel::cancelled());
        }
        let cmd = git_command(repo, args);
        let remaining = timeout.saturating_sub(started.elapsed());
        if remaining.is_zero() {
            return Err(format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64()).into());
        }
        // A hook-running command is cancellable and can write, so it is
        // never shared whatever the caller asked. A shared read observes its
        // flight (see [`FlightObserver`]); one that is not shared observes
        // its own request.
        let ran = if shared && stdin_bytes.is_none() && hooked.is_none() {
            run_read_shared(cmd, &label, remaining, stdout_cap)
        } else if let Some(cancel) = &cancel {
            let mut cmd = cmd;
            let mut observer = read_cancel::OwnCancel(cancel);
            run_observed(
                &mut cmd,
                &label,
                remaining,
                stdin_bytes,
                stdout_cap,
                &mut observer,
            )
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
            )
        } else {
            run_bounded_capped(cmd, &label, remaining, stdin_bytes, stdout_cap)
        };
        // A read cancelled while it queued for a slot has already come back as
        // the same cancellation as one cancelled while it ran
        // (`ProcessObserver::cancellation`), not a refusal with a label of its own.
        let mut out = ran?;
        if let Some((repo, _)) = &hooked {
            if out.cancelled {
                return Err(hooks::cancelled_message(repo, sub).into());
            }
            if let Some(Incomplete::Deadline { message, .. }) = &mut out.incomplete {
                *message = hooks::timeout_message(repo, sub, message);
            }
        } else if out.cancelled {
            // Stopped because nobody waits for it. Not a result: the output
            // is a partial stream, and an error is what keeps every caller
            // from parsing, keeping or sharing it as one.
            return Err(read_cancel::cancelled());
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
    parse_u64_saturating(text).map(|n| usize::try_from(n).unwrap_or(usize::MAX))
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
    use super::*;
    #[cfg(unix)]
    use crate::engine::process::read_cancel::ReadScope;
    #[cfg(unix)]
    use crate::engine::process::CANCELLED_MESSAGE;
    #[cfg(unix)]
    use crate::ipc_error::IpcErrorKind;
    use crate::procguard::LockedSpawn;
    #[cfg(unix)]
    use crate::test_support::wait_gone;
    #[cfg(unix)]
    use crate::test_support::wait_until;
    #[cfg(unix)]
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::thread;

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
    fn resolve_repo_attributes_its_rev_parse_to_its_own_source() {
        let dir = init_test_repo(false);
        let before = super::rev_parse_launches_from(super::RESOLVE_REPO_SOURCE);
        resolve_repo(&dir.path().to_string_lossy()).expect("resolve");
        assert!(
            super::rev_parse_launches_from(super::RESOLVE_REPO_SOURCE) > before,
            "resolve_repo's rev-parse was not attributed to {}",
            super::RESOLVE_REPO_SOURCE
        );
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
            "LC_MESSAGES",
            "LANGUAGE",
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
        assert_eq!(git["LC_MESSAGES"].as_deref(), Some("C"));
        assert_eq!(git["LANGUAGE"].as_deref(), Some("C"));
        // `LC_ALL` is explicitly removed (Some(None)), never set: it would
        // override the user's LC_CTYPE for every hook and tool git execs.
        assert_eq!(git.get("LC_ALL"), Some(&None), "git_command removes LC_ALL");
        assert_eq!(capture.get("LC_ALL"), git.get("LC_ALL"));
        assert_eq!(capture.get("LC_CTYPE"), git.get("LC_CTYPE"));
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
            assert_eq!(args[at + 2], "--no-textconv", "{patch}: {args:?}");
            // `diff` rejects `--no-show-signature`; `show` and `log` carry it
            // so a signature header is never printed (and verified) for them.
            if patch == "diff" {
                assert_eq!(args[at + 3], "HEAD", "{patch}: {args:?}");
            } else {
                assert_eq!(args[at + 3], "--no-show-signature", "{patch}: {args:?}");
                assert_eq!(args[at + 4], "HEAD", "{patch}: {args:?}");
            }
        }
        let notes_log = argv(&["-c", "x.y=z", "log", "-n1"]);
        let at = notes_log.iter().position(|a| a == "log").unwrap();
        assert_eq!(notes_log[at + 3], "--no-show-signature", "{notes_log:?}");
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
        // `blame` takes `--no-textconv` but rejects `--no-ext-diff`.
        let blame = argv(&["blame", "--line-porcelain", "--", "f"]);
        let at = blame.iter().position(|a| a == "blame").unwrap();
        assert_eq!(blame[at + 1], "--no-textconv", "{blame:?}");
        assert!(!blame.iter().any(|a| a == "--no-ext-diff"), "{blame:?}");
    }

    /// The textconv guard has one rule with `dc_proc::guarded_args`, which this
    /// crate does not depend on: read its source and require that every
    /// subcommand it adds `--no-textconv` after is one this builder does too,
    /// and the reverse. Also pins the locale rule of [`lc_ctype_from_inherited`].
    #[test]
    fn textconv_guard_matches_the_vendored_guarded_args() {
        let source = include_str!("../../vendored/dc-proc/src/lib.rs");
        let body = source
            .split("let guards: &[&str] = match sub.to_str() {")
            .nth(1)
            .and_then(|rest| rest.split("_ => &[],").next())
            .expect("guarded_args guard table");
        let mut vendored: Vec<&str> = ["diff", "log", "show", "blame"]
            .into_iter()
            .filter(|sub| {
                body.split("=>")
                    .zip(body.split("=>").skip(1))
                    .any(|(head, arm)| {
                        head.contains(&format!("\"{sub}\""))
                            && arm
                                .split("),")
                                .next()
                                .unwrap_or(arm)
                                .contains("--no-textconv")
                    })
            })
            .collect();
        vendored.sort_unstable();
        let mut ours: Vec<&str> = TEXTCONV_SUBCOMMANDS.to_vec();
        ours.sort_unstable();
        assert_eq!(ours, vendored, "textconv subcommands drifted from dc-proc");
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

    /// A repository with `siblings` linked worktrees, each dirty in a file of its
    /// own, and the files the edit is of: the ordinary state of a machine running
    /// parallel agents.
    #[cfg(unix)]
    struct Farm {
        _repo: tempfile::TempDir,
        _siblings: tempfile::TempDir,
        repo: String,
        dirs: Vec<std::path::PathBuf>,
    }

    #[cfg(unix)]
    fn farm(siblings: usize) -> Farm {
        use crate::test_support::git_in;
        let repo_dir = tempfile::tempdir().expect("repository");
        git_in(repo_dir.path(), &["init", "-b", "main"]);
        std::fs::write(repo_dir.path().join("seed.txt"), "seed").expect("seed");
        git_in(repo_dir.path(), &["add", "seed.txt"]);
        git_in(repo_dir.path(), &["commit", "-m", "seed"]);
        let repo = repo_dir
            .path()
            .canonicalize()
            .expect("canonical repository");
        let sibling_root = tempfile::tempdir().expect("sibling worktrees");
        let root = sibling_root.path().canonicalize().expect("canonical root");
        let mut dirs = vec![repo.clone()];
        for n in 0..siblings {
            let wt = root.join(format!("sibling-{n}"));
            git_in(
                &repo,
                &[
                    "worktree",
                    "add",
                    "-b",
                    &format!("b{n}"),
                    &wt.to_string_lossy(),
                ],
            );
            std::fs::write(wt.join(format!("own-{n}.txt")), "mine").expect("dirty a sibling");
            dirs.push(wt);
        }
        Farm {
            repo: repo.to_string_lossy().into_owned(),
            _repo: repo_dir,
            _siblings: sibling_root,
            dirs,
        }
    }

    /// AC1, characterization. One PreToolUse Edit through the production
    /// `collision-guard` (what `gitpulse-hook collision-guard` dispatches) against
    /// a real repository with two linked worktrees, and every git child it
    /// started, by where it ran and what it ran. Each child is one token of the
    /// shared spawn budget.
    ///
    /// The count is PINNED at what the code spends today. The cut (answer from the
    /// app over its socket, or cache the scan) lands with the hooks half of the
    /// card, behind ft-42081f16, and will lower this number deliberately.
    #[cfg(unix)]
    #[test]
    fn one_edit_hook_invocation_spends_this_many_git_spawns_by_call_site() {
        let farm = farm(2);
        let marks: Vec<usize> = farm
            .dirs
            .iter()
            .map(|dir| spawn_log::spawns_in(dir).len())
            .collect();
        let output = crate::hooks::run_collision_guard(&crate::hooks::HookInput {
            hook_event_name: "PreToolUse".to_string(),
            tool_name: "Edit".to_string(),
            cwd: farm.repo.clone(),
            file_path: format!("{}/seed.txt", farm.repo),
            ..Default::default()
        });
        let mut table: BTreeMap<(String, String), usize> = BTreeMap::new();
        let mut total = 0;
        for (dir, mark) in farm.dirs.iter().zip(&marks) {
            let place = if dir == &farm.dirs[0] {
                "main checkout".to_string()
            } else {
                format!("sibling {}", dir.file_name().unwrap().to_string_lossy())
            };
            for argv in &spawn_log::spawns_in(dir)[*mark..] {
                let args: Vec<&str> = argv.iter().map(String::as_str).collect();
                *table
                    .entry((place.clone(), subcommand(&args).to_string()))
                    .or_default() += 1;
                total += 1;
            }
        }
        println!("EDIT HOOK SPAWNS (3 worktrees, 1 PreToolUse Edit): {total} git children");
        println!("{:<18} {:<14} spawns", "where", "git command");
        for ((place, command), count) in &table {
            println!("{place:<18} {command:<14} {count}");
        }
        assert!(!output.is_silent() || total > 0, "the guard did not run");
        assert_eq!(
            total, 4,
            "the hook's spend changed: {table:?}; if this is the hooks-half cut, lower the pin"
        );
    }

    /// Through the real runner: the cancelled read is an error, so no caller
    /// parses its partial output, and `ref_cache` has nothing to keep.
    #[cfg(unix)]
    #[test]
    fn a_cancelled_git_read_is_an_error_and_never_an_answer() {
        for (shared, id) in [(true, "rc-git-shared"), (false, "rc-git-own")] {
            let repo = crate::test_support::git_repo();
            let dir = tempfile::TempDir::new().unwrap();
            let pids = dir.path().join("pids");
            let _reaper = crate::engine::process::read_cancel_tests::Reaper(pids.clone());
            // An alias that runs a shell: a slow git command the test controls.
            let alias = format!(
                "alias.rcslow=!{} >> '{}'; echo partial; exec sleep 60",
                crate::engine::process::read_cancel_tests::RECORD_SELF,
                pids.display()
            );
            let path = repo.path().to_path_buf();
            let reader = thread::spawn(move || {
                let scope = ReadScope::open(Some(id));
                let _in = scope.enter();
                let args = ["-c", alias.as_str(), "rcslow"];
                if shared {
                    git_text_shared(&path, &args).map(|text| (text, None))
                } else {
                    git_text_capped(&path, &args, 4096)
                }
            });
            assert!(
                wait_until(Duration::from_secs(10), || {
                    !crate::engine::process::read_cancel_tests::recorded(&pids).is_empty()
                }),
                "the git child never started"
            );
            let pid = crate::engine::process::read_cancel_tests::recorded(&pids)[0];
            assert!(read_cancel::cancel(id));
            let outcome = reader.join().unwrap();
            let error = outcome.expect_err("a cancelled read must not return its partial output");
            assert_eq!(error.message(), CANCELLED_MESSAGE, "shared={shared}");
            assert_eq!(error.kind(), IpcErrorKind::ReadCancelled, "shared={shared}");
            assert!(
                wait_gone(
                    pid,
                    crate::engine::process::read_cancel_tests::gone_within()
                ),
                "shared={shared}: pid {pid}"
            );
        }
    }

    /// A fallback after a failed read must not spawn for a caller who has gone.
    #[cfg(unix)]
    #[test]
    fn a_cancelled_request_starts_no_further_child() {
        let repo = crate::test_support::git_repo();
        let scope = ReadScope::open(Some("rc-no-more"));
        let _in = scope.enter();
        assert!(read_cancel::cancel("rc-no-more"));
        let (outcome, spawned) =
            spawn_labels_during(|| git_text(repo.path(), &["status", "--porcelain"]));
        let refused = outcome.unwrap_err();
        assert_eq!(refused.message(), CANCELLED_MESSAGE);
        assert_eq!(refused.kind(), IpcErrorKind::ReadCancelled);
        assert!(
            spawned.is_empty(),
            "spawned for a cancelled request: {spawned:?}"
        );
    }

    #[cfg(unix)]
    fn reactive_spawn_in(gate: &'static SpawnGate, cwd: &Path) -> Result<BoundedRun, CommandError> {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "true"]).current_dir(cwd);
        crate::engine::process::run_with_gate_options(
            &mut cmd,
            "credit-probe",
            crate::engine::process::RunOptions::new(Duration::from_millis(200)),
            None,
            1024,
            &mut (),
            gate,
        )
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
}
