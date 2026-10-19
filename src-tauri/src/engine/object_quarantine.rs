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
    git_captured_quarantined, is_run_timeout, resolve_git_common_dir, BoundedRun, DEFAULT_TIMEOUT,
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
    /// conflicted paths in it.
    Conflicted { tree: String },
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
        let run = self.run(
            repo,
            &["merge-tree", "--write-tree", "--no-messages", ours, theirs],
            deadline,
        )?;
        let stdout = String::from_utf8_lossy(&run.stdout);
        let tree = stdout.lines().next().unwrap_or_default().trim();
        match run.status_code {
            0 | 1 if is_object_name(tree) => Ok(if run.status_code == 0 {
                MergeOutcome::Clean {
                    tree: tree.to_string(),
                }
            } else {
                MergeOutcome::Conflicted {
                    tree: tree.to_string(),
                }
            }),
            _ => {
                let stderr = String::from_utf8_lossy(&run.stderr);
                let stderr = stderr.trim();
                // git before 2.38 rejects the option itself.
                if (stderr.contains("write-tree") && stderr.contains("unknown option"))
                    || stderr.contains("usage: git merge-tree")
                {
                    Err(QuarantineError::Unsupported(stderr.to_string()))
                } else {
                    Err(QuarantineError::Git(format!(
                        "git merge-tree exited {}: {stderr}",
                        run.status_code
                    )))
                }
            }
        }
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
        let mut builder = std::fs::DirBuilder::new();
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
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
