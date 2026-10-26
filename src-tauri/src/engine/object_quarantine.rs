//! A quarantined object directory: run a git command that must write objects
//! (`git merge-tree --write-tree`) without the repository's object database
//! gaining a byte.
//!
//! `merge-tree --write-tree` computes a merge without touching the index or the
//! working tree, but it writes the merged trees (and, for a conflicted file,
//! blobs) as loose objects. The read tools promise "never mutates git state"
//! (owner decision 2), so those writes are pointed somewhere else:
//!
//! - `GIT_OBJECT_DIRECTORY` is a fresh directory this module owns, so every
//!   object git writes lands there;
//! - `GIT_ALTERNATE_OBJECT_DIRECTORIES` names the repository's own object
//!   directory, so git still *reads* everything the repository has (git also
//!   follows that directory's own `info/alternates`);
//! - the directory is removed when the [`ObjectQuarantine`] is dropped, which
//!   covers a normal return, an early `?`, a deadline and a panic unwinding.
//!
//! The commands run through [`crate::engine::git_cli`], which owns the spawn
//! gate, the output caps, the scrubbed environment and the repository-trust
//! check; the redirect is the one environment override that seam accepts, and
//! the deadline is the caller's. A merge in a repository that cannot be read
//! through alternates (a partial clone, whose missing blobs git would fetch
//! *into the quarantine*) is the caller's to refuse before getting here.
//!
//! Users: the `tree` route of `gitpulse_landed` today; the conflict engine and
//! the object-database integration are meant to import this module rather than
//! write a second quarantine.

use crate::engine::git_cli::{
    git_captured, git_captured_quarantined, git_text_with_timeout, is_run_timeout,
    resolve_git_common_dir, BoundedRun, DEFAULT_TIMEOUT,
};
use std::ffi::OsString;
use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Why a quarantined command did not produce an answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QuarantineError {
    /// The quarantine directory or the alternates could not be set up. Nothing
    /// was run.
    Setup(String),
    /// The caller's deadline passed, before or while git ran.
    DeadlineExceeded,
    /// The installed git does not have `merge-tree --write-tree` (before 2.38).
    Unsupported(String),
    /// git could not be spawned (the gate refused it, or it is missing), was
    /// cut by the output cap, or failed with a status that is not an answer.
    Git(String),
}

impl fmt::Display for QuarantineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => write!(f, "object quarantine unavailable: {reason}"),
            Self::DeadlineExceeded => write!(f, "object quarantine deadline exceeded"),
            Self::Unsupported(reason) => write!(f, "merge-tree --write-tree unavailable: {reason}"),
            Self::Git(reason) => write!(f, "{reason}"),
        }
    }
}

impl std::error::Error for QuarantineError {}

/// What `git merge-tree --write-tree` concluded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergeOutcome {
    /// The merge is clean; `tree` is the merged tree.
    Clean { tree: String },
    /// The merge has conflicts; `tree` is the tree git wrote with the
    /// conflicted paths in it. `paths` is the first [`MAX_CONFLICT_PATHS`] of
    /// them, in the order git listed them, and `paths_total` how many git named.
    Conflicted {
        tree: String,
        paths: Vec<String>,
        paths_total: usize,
    },
}

/// Most conflicted paths one outcome carries. The count past it is kept in
/// `paths_total`, so a cut list is never read as the whole list.
pub const MAX_CONFLICT_PATHS: usize = 200;

/// What a merge of two commits would do, as the conflict engine reports it:
/// three states, and the third is not a flavour of the first.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum MergeCandidate {
    /// The merge is clean; `tree` is the merged tree (it exists only in the
    /// quarantine, which has since been deleted).
    Clean { tree: String },
    /// The merge conflicts in `paths` (the first [`MAX_CONFLICT_PATHS`]);
    /// `paths_total` is how many git named.
    Conflict {
        paths: Vec<String>,
        paths_total: usize,
    },
    /// Nothing was established: a git failure, unrelated histories, a
    /// deadline, a git without `merge-tree --write-tree`, a partial clone, or
    /// an operand that is not a commit. Never to be read as `Clean`.
    Unknown { reason: String },
}

/// A scratch object directory that is deleted when this value is dropped.
#[derive(Debug)]
pub struct ObjectQuarantine {
    dir: PathBuf,
    alternates: OsString,
}

static SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Attempts at a fresh name before giving up: a collision needs the same pid,
/// counter and nanosecond clock, so more than one is already a broken clock.
const NAME_ATTEMPTS: usize = 8;

impl ObjectQuarantine {
    /// A quarantine for `repo`: an empty private directory under the system
    /// temporary directory, reading through to the repository's objects.
    pub fn new(repo: &Path) -> Result<Self, QuarantineError> {
        let objects = resolve_git_common_dir(repo)
            .map_err(QuarantineError::Setup)?
            .join("objects");
        if !objects.is_absolute() || !objects.is_dir() {
            return Err(QuarantineError::Setup(format!(
                "{} is not the repository's object directory",
                objects.display()
            )));
        }
        // `join_paths` refuses a path holding the platform's list separator,
        // which git could not read back as one directory.
        let alternates = std::env::join_paths([&objects])
            .map_err(|e| QuarantineError::Setup(format!("alternates: {e}")))?;
        // A crash or a SIGKILL skips the Drop guard and leaves a directory
        // behind; the next quarantine clears the old ones. Best effort: a
        // sweep that cannot finish never stops this one from being made.
        sweep_stale(&std::env::temp_dir(), STALE_AFTER);
        Ok(Self {
            dir: create_private_dir()?,
            alternates,
        })
    }

    /// The scratch directory every object written through this quarantine
    /// lands in.
    pub fn path(&self) -> &Path {
        &self.dir
    }

    /// Runs `git <args>` in `repo` with its object writes redirected here.
    /// Exit status is the caller's to interpret; only a spawn failure, a
    /// deadline or a truncated stream is an error.
    pub(crate) fn run(
        &self,
        repo: &Path,
        args: &[&str],
        deadline: Instant,
    ) -> Result<BoundedRun, QuarantineError> {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(QuarantineError::DeadlineExceeded);
        }
        let timeout = remaining.min(DEFAULT_TIMEOUT);
        let run = git_captured_quarantined(repo, args, &self.dir, &self.alternates, timeout)
            .map_err(|e| {
                if is_run_timeout(&e) {
                    QuarantineError::DeadlineExceeded
                } else {
                    QuarantineError::Git(e)
                }
            })?;
        run.require_complete("git").map_err(QuarantineError::Git)
    }

    /// `git merge-tree --write-tree <ours> <theirs>`: the merge of two commits,
    /// computed without an index or working tree. Both operands must be full
    /// object names (hex); anything else is refused, so a caller's text can
    /// never become an option.
    pub fn merge_tree(
        &self,
        repo: &Path,
        ours: &str,
        theirs: &str,
        deadline: Instant,
    ) -> Result<MergeOutcome, QuarantineError> {
        for operand in [ours, theirs] {
            if !is_object_name(operand) {
                return Err(QuarantineError::Git(format!(
                    "merge-tree operand {operand:?} is not a full object name"
                )));
            }
        }
        // `-z` makes the output NUL-terminated, so a path with a newline or a
        // quote in it is one name rather than several or an escaped one.
        // `--name-only` lists each conflicted path once after the tree, and
        // `--no-messages` ends the output there.
        let run = self.run(
            repo,
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "-z",
                ours,
                theirs,
            ],
            deadline,
        )?;
        // Bytes, not text: a path git names need not be UTF-8, and a lossy
        // conversion would make two different names the same string.
        let mut fields = run.stdout.split(|byte| *byte == 0);
        let tree = String::from_utf8_lossy(fields.next().unwrap_or_default());
        let tree = tree.trim();
        match run.status_code {
            0 | 1 if is_object_name(tree) => {
                if run.status_code == 0 {
                    return Ok(MergeOutcome::Clean {
                        tree: tree.to_string(),
                    });
                }
                let named: Vec<String> = fields
                    .filter(|path| !path.is_empty())
                    .map(render_path)
                    .collect();
                // A conflict that names no path is not an answer a caller can
                // act on, and it is not a clean merge either.
                if named.is_empty() {
                    return Err(QuarantineError::Git(
                        "git merge-tree reported a conflict and named no path".to_string(),
                    ));
                }
                let paths_total = named.len();
                Ok(MergeOutcome::Conflicted {
                    tree: tree.to_string(),
                    paths: named.into_iter().take(MAX_CONFLICT_PATHS).collect(),
                    paths_total,
                })
            }
            _ => Err(classify_failure(
                run.status_code,
                String::from_utf8_lossy(&run.stderr).trim(),
            )),
        }
    }
}

/// A conflicted path as text, losslessly: valid UTF-8 as it is, a byte that is
/// not UTF-8 as a three-digit octal escape (`\377`, as git quotes it), and a
/// backslash doubled so that an escape and a name that spells one never
/// collide. Two different paths are always two different strings.
fn render_path(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        for ch in chunk.valid().chars() {
            if ch == '\\' {
                out.push_str("\\\\");
            } else {
                out.push(ch);
            }
        }
        for byte in chunk.invalid() {
            out.push_str(&format!("\\{byte:03o}"));
        }
    }
    out
}

/// git before 2.38 rejects the option itself; every other failure is a failed
/// run.
fn classify_failure(status_code: i32, stderr: &str) -> QuarantineError {
    if (stderr.contains("write-tree") && stderr.contains("unknown option"))
        || stderr.contains("usage: git merge-tree")
    {
        QuarantineError::Unsupported(stderr.to_string())
    } else {
        QuarantineError::Git(format!("git merge-tree exited {status_code}: {stderr}"))
    }
}

/// What merging `head` into `base` would do, without the repository gaining
/// an object: the one conflict primitive (the merged-by-tree check of dead
/// branches and the conflict read tool both call it).
///
/// Both operands are resolved to full commit names first, in the repository
/// and not in the quarantine, so a ref name, an abbreviation or a tag all work
/// and nothing a caller wrote can become an option. A partial clone is refused
/// before anything runs (git would fetch the missing blobs into the
/// quarantine, from the network), and lazy fetch is off besides. Every
/// failure is `Unknown` with its reason: unrelated histories, a deadline, a
/// refused spawn, a git older than 2.38.
pub fn merge_tree_candidate(
    repo: &Path,
    base: &str,
    head: &str,
    deadline: Instant,
) -> MergeCandidate {
    crate::engine::git_cli::without_lazy_fetch(|| {
        refuse_partial_clone(repo)?;
        let base = resolve_commit(repo, base, deadline)?;
        let head = resolve_commit(repo, head, deadline)?;
        Ok(merge_resolved(repo, &base, &head, deadline))
    })
    .unwrap_or_else(|reason: String| MergeCandidate::Unknown { reason })
}

/// Err when `repo` is a partial clone, or when that could not be told: a merge
/// there reads file contents, and git would fetch the missing ones from the
/// remote. A caller that merges many pairs asks once and then calls
/// [`merge_tree_resolved`].
pub(crate) fn refuse_partial_clone(repo: &Path) -> Result<(), String> {
    match is_partial_clone(repo) {
        Ok(false) => Ok(()),
        Ok(true) => Err(
            "this repository is a partial clone: a merge reads file contents \
             and git would fetch missing ones from the remote; nothing was run"
                .to_string(),
        ),
        Err(reason) => Err(format!(
            "could not tell whether this is a partial clone, so nothing was run: {reason}"
        )),
    }
}

/// [`merge_tree_candidate`] for operands that are already full commit names,
/// in a repository the caller has already checked with
/// [`refuse_partial_clone`]: one quarantine and one `merge-tree` per call.
pub(crate) fn merge_tree_resolved(
    repo: &Path,
    base: &str,
    head: &str,
    deadline: Instant,
) -> MergeCandidate {
    crate::engine::git_cli::without_lazy_fetch(|| merge_resolved(repo, base, head, deadline))
}

fn merge_resolved(repo: &Path, base: &str, head: &str, deadline: Instant) -> MergeCandidate {
    let answer = ObjectQuarantine::new(repo)
        .and_then(|quarantine| quarantine.merge_tree(repo, base, head, deadline));
    match answer {
        Ok(MergeOutcome::Clean { tree }) => MergeCandidate::Clean { tree },
        Ok(MergeOutcome::Conflicted {
            paths, paths_total, ..
        }) => MergeCandidate::Conflict { paths, paths_total },
        Err(e) => MergeCandidate::Unknown {
            reason: e.to_string(),
        },
    }
}

/// The full commit name `operand` names in `repo`.
fn resolve_commit(repo: &Path, operand: &str, deadline: Instant) -> Result<String, String> {
    if operand.is_empty() || operand.starts_with('-') {
        return Err(format!("{operand:?} is not a commit name"));
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(QuarantineError::DeadlineExceeded.to_string());
    }
    let text = git_text_with_timeout(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("{operand}^{{commit}}"),
        ],
        remaining.min(DEFAULT_TIMEOUT),
    )
    .map_err(|e| {
        if is_run_timeout(&e) {
            QuarantineError::DeadlineExceeded.to_string()
        } else {
            format!("could not resolve {operand:?} to a commit: {e}")
        }
    })?;
    let oid = text.trim();
    if is_object_name(oid) {
        Ok(oid.to_string())
    } else {
        Err(format!("{operand:?} did not resolve to a commit"))
    }
}

/// Whether the repository is a partial clone (`extensions.partialclone` or a
/// promisor remote). Read from configuration only, so it fetches nothing.
pub(crate) fn is_partial_clone(repo: &Path) -> Result<bool, String> {
    // Git decides what a boolean is (`true`, `yes`, `on`, `1`, any case, and a
    // bare key), so it is asked with `--type=bool` rather than parsed here.
    // A value it cannot read as a boolean is a failed read, which refuses: a
    // config that could not be read is never "not partial".
    let promisors = git_captured(
        repo,
        &[
            "config",
            "--type=bool",
            "--get-regexp",
            r"^remote\..*\.promisor$",
        ],
    )?
    .require_complete("git config")?;
    match (promisors.success, promisors.status_code) {
        (true, _) => {
            if String::from_utf8_lossy(&promisors.stdout)
                .lines()
                .any(|line| line.rsplit(' ').next() == Some("true"))
            {
                return Ok(true);
            }
        }
        // Exit 1 with nothing on stderr is "no such key".
        (false, 1) if promisors.stderr.is_empty() && !promisors.cancelled => {}
        _ => {
            return Err(format!(
                "git config failed: {}",
                String::from_utf8_lossy(&promisors.stderr).trim()
            ))
        }
    }
    // `extensions.partialclone` names the promisor remote: set means partial.
    let extension = git_captured(repo, &["config", "--get", "extensions.partialclone"])?
        .require_complete("git config")?;
    match (extension.success, extension.status_code) {
        (true, _) => Ok(!String::from_utf8_lossy(&extension.stdout).trim().is_empty()),
        (false, 1) if extension.stderr.is_empty() && !extension.cancelled => Ok(false),
        _ => Err(format!(
            "git config failed: {}",
            String::from_utf8_lossy(&extension.stderr).trim()
        )),
    }
}

impl Drop for ObjectQuarantine {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.dir) {
            if e.kind() != std::io::ErrorKind::NotFound {
                log::warn!(
                    "object quarantine {} was not removed: {e}",
                    self.dir.display()
                );
            }
        }
    }
}

/// What every quarantine directory's name starts with.
const NAME_PREFIX: &str = "gitpulse-quarantine-";

/// A quarantine only lives for one command (a minute at most), so one this old
/// belongs to a process that died without running its Drop guard.
const STALE_AFTER: Duration = Duration::from_secs(60 * 60);

/// Most leftovers removed by one sweep, and most directory entries looked at:
/// a temp directory of millions of files costs one sweep a bounded read.
const SWEEP_REMOVALS: usize = 32;
const SWEEP_ENTRIES: usize = 10_000;

/// Removes quarantine directories under `base` that are older than
/// `older_than`: named exactly as [`create_private_dir`] names them (prefix and
/// digit groups), a real directory (never a symlink), and owned by this user.
/// Returns how many were removed.
pub(crate) fn sweep_stale(base: &Path, older_than: Duration) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.take(SWEEP_ENTRIES).flatten() {
        if removed >= SWEEP_REMOVALS {
            break;
        }
        let name = entry.file_name();
        let Some(rest) = name.to_str().and_then(|n| n.strip_prefix(NAME_PREFIX)) else {
            continue;
        };
        if rest.is_empty() || !rest.bytes().all(|b| b.is_ascii_digit() || b == b'-') {
            continue;
        }
        let path = entry.path();
        // `symlink_metadata`: a link planted under this name is not followed.
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || !owned_by_this_user(&meta) {
            continue;
        }
        let old_enough = meta
            .modified()
            .ok()
            .and_then(|modified| modified.elapsed().ok())
            .is_some_and(|age| age >= older_than);
        if !old_enough {
            continue;
        }
        match std::fs::remove_dir_all(&path) {
            Ok(()) => removed += 1,
            Err(e) => log::warn!(
                "stale object quarantine {} not removed: {e}",
                path.display()
            ),
        }
    }
    removed
}

#[cfg(unix)]
fn owned_by_this_user(meta: &std::fs::Metadata) -> bool {
    // SAFETY: `geteuid` reads this process's own credentials and cannot fail.
    std::os::unix::fs::MetadataExt::uid(meta) == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn owned_by_this_user(_meta: &std::fs::Metadata) -> bool {
    true
}

fn is_object_name(text: &str) -> bool {
    matches!(text.len(), 40 | 64) && text.bytes().all(|b| b.is_ascii_hexdigit())
}

fn create_private_dir() -> Result<PathBuf, QuarantineError> {
    let base = std::env::temp_dir();
    let mut last = String::new();
    for _ in 0..NAME_ATTEMPTS {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or(Duration::ZERO)
            .as_nanos();
        let name = format!(
            "gitpulse-quarantine-{}-{}-{nanos}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        );
        let path = base.join(name);
        #[cfg(unix)]
        let builder = {
            let mut builder = std::fs::DirBuilder::new();
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            builder
        };
        #[cfg(not(unix))]
        let builder = std::fs::DirBuilder::new();
        match builder.create(&path) {
            Ok(()) => return Ok(path),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = e.to_string(),
            Err(e) => {
                return Err(QuarantineError::Setup(format!(
                    "create {}: {e}",
                    path.display()
                )))
            }
        }
    }
    Err(QuarantineError::Setup(format!(
        "no unused name after {NAME_ATTEMPTS} attempts: {last}"
    )))
}

#[cfg(test)]
#[path = "object_quarantine_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "object_quarantine_adv_tests.rs"]
mod adv_tests;
