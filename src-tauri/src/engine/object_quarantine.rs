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
//! The commands run through [`crate::engine::git_cli`] (over [`crate::engine::process`]), which enforce the spawn
//! gate, the output caps, the scrubbed environment and the repository-trust
//! check; the redirect is the one environment override that seam accepts, and
//! the deadline is the caller's. A merge in a repository that cannot be read
//! through alternates (a partial clone, whose missing blobs git would fetch
//! *into the quarantine*) is the caller's to refuse before getting here.
//!
//! Users: the `tree` route of `gitpulse_landed` today, and the conflict engine.
//! Lane integration reuses [`parse_merge_tree_output`] and writes the merged
//! tree into the repository's own object database: a tree that exists only in
//! this quarantine cannot be committed there, and the quarantine is deleted
//! with this value.

use crate::engine::git_cli::{
    git_captured, git_captured_quarantined, git_spawn_quarantined_streaming, git_text_with_timeout,
    resolve_git_common_dir, without_lazy_fetch,
};
use crate::engine::process::{is_run_timeout, BoundedRun, StreamingChild, DEFAULT_TIMEOUT};
use crate::ipc_error::CommandError;
use std::ffi::OsString;
use std::fmt;
use std::io::{BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout};
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
    Git(CommandError),
}

impl fmt::Display for QuarantineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Setup(reason) => write!(f, "object quarantine unavailable: {reason}"),
            Self::DeadlineExceeded => write!(f, "object quarantine deadline exceeded"),
            Self::Unsupported(reason) => write!(f, "merge-tree --write-tree unavailable: {reason}"),
            Self::Git(reason) => f.write_str(reason.message()),
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

/// Most times one [`MergeTreeStream`] respawns after an unexpected child exit
/// (such as an unrelated-history `rc=128` or missing-OID `rc=1`) within one batch.
pub const MAX_STREAM_RESPAWNS: usize = 4;

/// Maximum path count accepted for a single conflict-message group in
/// `git merge-tree --messages` output (closes negative/overflow `atoi` bugs).
pub const MAX_MESSAGE_PATH_COUNT: usize = 1024;

/// Upper bound on bytes buffered for a single NUL-NUL stream record.
const MAX_STREAM_RECORD_BYTES: usize = 16 * 1024 * 1024;

/// Upper bound on stderr bytes read from an exited stream child.
const MAX_STREAM_STDERR_BYTES: usize = 64 * 1024;

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
            .map_err(|e| QuarantineError::Setup(e.into_message()))?
            .join("objects");
        if !objects.is_absolute() || !objects.is_dir() {
            return Err(QuarantineError::Setup(format!(
                "{} is not the repository's object directory",
                objects.display()
            )));
        }
        // `join_paths` refuses a path holding the platform's list separator,
        // which git could not read back as one directory. On Windows it does
        // not refuse a `;`: it wraps the path in quotes, which git reads as a
        // C-style string, where each `\` of the path starts an escape and the
        // result names another directory. That is refused here instead.
        let alternates = std::env::join_paths([&objects])
            .map_err(|e| QuarantineError::Setup(format!("alternates: {e}")))?;
        if cfg!(windows) && objects.to_string_lossy().contains(';') {
            return Err(QuarantineError::Setup(format!(
                "alternates: {} holds the list separator ';'",
                objects.display()
            )));
        }
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
                if is_run_timeout(e.message()) {
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
                return Err(QuarantineError::Git(
                    format!("merge-tree operand {operand:?} is not a full object name").into(),
                ));
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
        parse_merge_tree_output(
            &run.stdout,
            run.status_code,
            &String::from_utf8_lossy(&run.stderr),
        )
    }

    /// Opens a batch-scoped `git merge-tree --stdin --write-tree --name-only --no-messages -z`
    /// stream inside a fresh object quarantine, holding one `SpawnPermit` and
    /// one rate token for the batch.
    ///
    /// Never kept globally idle across batches: dropping the returned
    /// [`MergeTreeStream`] kills and reaps any live child, releases the permit,
    /// and deletes the quarantine directory.
    pub fn open_stream(
        repo: &Path,
        batch_deadline: Instant,
    ) -> Result<MergeTreeStream, QuarantineError> {
        if Instant::now() >= batch_deadline {
            return Err(QuarantineError::DeadlineExceeded);
        }
        let quarantine = Self::new(repo)?;
        let live =
            without_lazy_fetch(|| spawn_merge_tree_stream(repo, &quarantine, batch_deadline))?;
        Ok(MergeTreeStream {
            repo: repo.to_path_buf(),
            quarantine,
            batch_deadline,
            live: Some(live),
            respawns: 0,
            spawns: 1,
            single_shot_fallback: false,
        })
    }
}

struct LiveStream {
    child: StreamingChild,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: ChildStdout,
    stderr: Option<ChildStderr>,
    read_buf: Vec<u8>,
    read_pos: usize,
}

enum StreamReadError {
    Deadline,
    UnexpectedEof(String),
    Malformed(String),
}

/// Batch-scoped streaming `git merge-tree --stdin` session inside one
/// [`ObjectQuarantine`].
pub struct MergeTreeStream {
    repo: PathBuf,
    quarantine: ObjectQuarantine,
    batch_deadline: Instant,
    live: Option<LiveStream>,
    respawns: usize,
    spawns: usize,
    single_shot_fallback: bool,
}

impl MergeTreeStream {
    /// Path of the underlying scratch quarantine object directory.
    #[cfg(test)]
    pub(crate) fn quarantine_path(&self) -> &Path {
        self.quarantine.path()
    }

    /// Number of child respawns performed after unexpected stream exits.
    #[cfg(test)]
    pub(crate) fn respawn_count(&self) -> usize {
        self.respawns
    }

    /// Total number of `merge-tree --stdin` processes spawned in this batch.
    #[cfg(test)]
    pub(crate) fn spawn_count(&self) -> usize {
        self.spawns
    }

    /// Merges two full-OID commits using the batch stream (or single-shot fallback
    /// if Git lacks `--stdin`).
    pub fn merge_tree(
        &mut self,
        ours: &str,
        theirs: &str,
    ) -> Result<MergeOutcome, QuarantineError> {
        self.merge_tree_with_base(None, ours, theirs, self.batch_deadline)
    }

    /// [`Self::merge_tree`] bounded by `pair_deadline.min(batch_deadline)`.
    pub fn merge_tree_until(
        &mut self,
        ours: &str,
        theirs: &str,
        pair_deadline: Instant,
    ) -> Result<MergeOutcome, QuarantineError> {
        self.merge_tree_with_base(None, ours, theirs, pair_deadline)
    }

    /// Merges `ours` and `theirs` (optionally against explicit `base`) in strict
    /// 1-at-a-time lockstep.
    pub fn merge_tree_with_base(
        &mut self,
        base: Option<&str>,
        ours: &str,
        theirs: &str,
        pair_deadline: Instant,
    ) -> Result<MergeOutcome, QuarantineError> {
        for operand in base.into_iter().chain([ours, theirs]) {
            if !is_object_name(operand) {
                return Err(QuarantineError::Git(
                    format!("merge-tree operand {operand:?} is not a full object name").into(),
                ));
            }
        }
        let effective_deadline = pair_deadline.min(self.batch_deadline);
        if Instant::now() >= effective_deadline {
            return Err(QuarantineError::DeadlineExceeded);
        }
        if self.single_shot_fallback {
            return self.run_single_shot(base, ours, theirs, effective_deadline);
        }
        without_lazy_fetch(|| self.merge_stream_step(base, ours, theirs, effective_deadline))
    }

    /// [`Self::merge_tree`] mapped into [`MergeCandidate`].
    pub fn merge_candidate(&mut self, ours: &str, theirs: &str) -> MergeCandidate {
        self.merge_candidate_until(ours, theirs, self.batch_deadline)
    }

    /// [`Self::merge_tree_until`] mapped into [`MergeCandidate`].
    pub fn merge_candidate_until(
        &mut self,
        ours: &str,
        theirs: &str,
        pair_deadline: Instant,
    ) -> MergeCandidate {
        match self.merge_tree_until(ours, theirs, pair_deadline) {
            Ok(MergeOutcome::Clean { tree }) => MergeCandidate::Clean { tree },
            Ok(MergeOutcome::Conflicted {
                paths, paths_total, ..
            }) => MergeCandidate::Conflict { paths, paths_total },
            Err(e) => MergeCandidate::Unknown {
                reason: e.to_string(),
            },
        }
    }

    /// Resolves `head_or_ref` if it is not already a full object name, then
    /// merges it against full OID `base` via the stream.
    pub(crate) fn merge_candidate_or_resolve(
        &mut self,
        base: &str,
        head_or_ref: &str,
    ) -> MergeCandidate {
        let resolved = if is_object_name(head_or_ref) {
            head_or_ref.to_string()
        } else {
            match without_lazy_fetch(|| {
                resolve_commit(&self.repo, head_or_ref, self.batch_deadline)
            }) {
                Ok(oid) => oid,
                Err(e) => {
                    return MergeCandidate::Unknown {
                        reason: e.into_message(),
                    };
                }
            }
        };
        self.merge_candidate(base, &resolved)
    }

    fn merge_stream_step(
        &mut self,
        base: Option<&str>,
        ours: &str,
        theirs: &str,
        effective_deadline: Instant,
    ) -> Result<MergeOutcome, QuarantineError> {
        if self.live.is_none() {
            if self.respawns >= MAX_STREAM_RESPAWNS {
                return Err(QuarantineError::Git(
                    format!("git merge-tree stream exceeded {MAX_STREAM_RESPAWNS} respawns").into(),
                ));
            }
            self.respawns += 1;
            let live = spawn_merge_tree_stream(&self.repo, &self.quarantine, self.batch_deadline)?;
            self.spawns += 1;
            self.live = Some(live);
        }

        let line = match base {
            Some(base_oid) => format!("{base_oid} -- {ours} {theirs}\n"),
            None => format!("{ours} {theirs}\n"),
        };
        let write_err = {
            let live = self.live.as_mut().expect("live stream initialized");
            let stdin = live.stdin.as_mut().expect("stream stdin present");
            stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush())
                .err()
        };
        if let Some(err) = write_err {
            return self.handle_stream_death(
                base,
                ours,
                theirs,
                effective_deadline,
                &err.to_string(),
            );
        }

        let read_result = {
            let live = self.live.as_mut().expect("live stream present");
            read_stream_record(live, effective_deadline)
        };
        match read_result {
            Ok(outcome) => Ok(outcome),
            Err(StreamReadError::Deadline) => {
                if let Some(mut live) = self.live.take() {
                    drop(live.stdin.take());
                    let _ = live.child.kill_and_reap();
                }
                Err(QuarantineError::DeadlineExceeded)
            }
            Err(StreamReadError::UnexpectedEof(detail) | StreamReadError::Malformed(detail)) => {
                self.handle_stream_death(base, ours, theirs, effective_deadline, &detail)
            }
        }
    }

    fn handle_stream_death(
        &mut self,
        base: Option<&str>,
        ours: &str,
        theirs: &str,
        effective_deadline: Instant,
        detail: &str,
    ) -> Result<MergeOutcome, QuarantineError> {
        let Some(mut live) = self.live.take() else {
            return Err(QuarantineError::Git(detail.to_string().into()));
        };
        drop(live.stdin.take());
        let status_code = live
            .child
            .wait_and_reap()
            .ok()
            .and_then(|status| status.code())
            .unwrap_or(-1);
        let stderr = read_bounded_stderr(live.stderr.take());
        let trimmed = stderr.trim();
        if status_code == 129
            || matches!(
                classify_failure(status_code, trimmed),
                QuarantineError::Unsupported(_)
            )
        {
            self.single_shot_fallback = true;
            return self.run_single_shot(base, ours, theirs, effective_deadline);
        }
        let reason = if !trimmed.is_empty() {
            trimmed.to_string()
        } else {
            format!("git merge-tree --stdin exited {status_code}: {detail}")
        };
        Err(QuarantineError::Git(reason.into()))
    }

    fn run_single_shot(
        &self,
        base: Option<&str>,
        ours: &str,
        theirs: &str,
        effective_deadline: Instant,
    ) -> Result<MergeOutcome, QuarantineError> {
        without_lazy_fetch(|| match base {
            None => self
                .quarantine
                .merge_tree(&self.repo, ours, theirs, effective_deadline),
            Some(base_oid) => {
                let run = self.quarantine.run(
                    &self.repo,
                    &[
                        "merge-tree",
                        "--write-tree",
                        "--name-only",
                        "--no-messages",
                        "-z",
                        "--merge-base",
                        base_oid,
                        ours,
                        theirs,
                    ],
                    effective_deadline,
                )?;
                parse_merge_tree_output(
                    &run.stdout,
                    run.status_code,
                    &String::from_utf8_lossy(&run.stderr),
                )
            }
        })
    }
}

fn spawn_merge_tree_stream(
    repo: &Path,
    quarantine: &ObjectQuarantine,
    batch_deadline: Instant,
) -> Result<LiveStream, QuarantineError> {
    let remaining = batch_deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(QuarantineError::DeadlineExceeded);
    }
    let timeout = remaining.min(DEFAULT_TIMEOUT);
    let mut child = git_spawn_quarantined_streaming(
        repo,
        &[
            "merge-tree",
            "--stdin",
            "--write-tree",
            "--name-only",
            "--no-messages",
            "-z",
        ],
        &quarantine.dir,
        &quarantine.alternates,
        timeout,
    )
    .map_err(|e| {
        if is_run_timeout(e.message()) {
            QuarantineError::DeadlineExceeded
        } else {
            QuarantineError::Git(e)
        }
    })?;
    let Some(stdin) = child.take_stdin() else {
        return Err(QuarantineError::Setup(
            "merge-tree --stdin missing stdin pipe".into(),
        ));
    };
    let Some(stdout) = child.take_stdout() else {
        return Err(QuarantineError::Setup(
            "merge-tree --stdin missing stdout pipe".into(),
        ));
    };
    let stderr = child.take_stderr();
    Ok(LiveStream {
        child,
        stdin: Some(BufWriter::new(stdin)),
        stdout,
        stderr,
        read_buf: Vec::new(),
        read_pos: 0,
    })
}

fn read_bounded_stderr(stderr: Option<ChildStderr>) -> String {
    let Some(pipe) = stderr else {
        return String::new();
    };
    let mut buf = Vec::new();
    let _ = pipe
        .take(MAX_STREAM_STDERR_BYTES as u64)
        .read_to_end(&mut buf);
    String::from_utf8_lossy(&buf).into_owned()
}

fn read_nul_field(
    live: &mut LiveStream,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, StreamReadError> {
    if live.read_pos > 0 && (live.read_pos == live.read_buf.len() || live.read_pos >= 8192) {
        live.read_buf.drain(..live.read_pos);
        live.read_pos = 0;
    }
    let mut scratch = [0u8; 4096];
    loop {
        if let Some(rel) = live.read_buf[live.read_pos..]
            .iter()
            .position(|&byte| byte == 0)
        {
            let field = live.read_buf[live.read_pos..live.read_pos + rel].to_vec();
            live.read_pos += rel + 1;
            return Ok(Some(field));
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(StreamReadError::Deadline);
        }
        match live.child.wait_stdout_readable(&live.stdout, remaining) {
            Ok(true) => {}
            Ok(false) => continue,
            Err(e) => return Err(StreamReadError::UnexpectedEof(e.to_string())),
        }
        match live.stdout.read(&mut scratch) {
            Ok(0) => {
                if live.read_pos < live.read_buf.len() {
                    return Err(StreamReadError::UnexpectedEof(
                        "git merge-tree --stdin closed stdout mid-record".into(),
                    ));
                }
                return Ok(None);
            }
            Ok(n) => {
                let buffered = live.read_buf.len().saturating_sub(live.read_pos);
                if buffered.saturating_add(n) > MAX_STREAM_RECORD_BYTES {
                    return Err(StreamReadError::Malformed(format!(
                        "git merge-tree --stdin record exceeded {MAX_STREAM_RECORD_BYTES} bytes"
                    )));
                }
                live.read_buf.extend_from_slice(&scratch[..n]);
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(StreamReadError::UnexpectedEof(e.to_string())),
        }
    }
}

fn read_stream_record(
    live: &mut LiveStream,
    deadline: Instant,
) -> Result<MergeOutcome, StreamReadError> {
    let status = read_nul_field(live, deadline)?
        .ok_or_else(|| StreamReadError::UnexpectedEof("unexpected EOF".into()))?;
    let clean = match status.as_slice() {
        b"1" => true,
        b"0" => false,
        other => {
            return Err(StreamReadError::Malformed(format!(
                "unexpected merge-tree stream status {:?}",
                String::from_utf8_lossy(other)
            )));
        }
    };
    let tree_bytes = read_nul_field(live, deadline)?.ok_or_else(|| {
        StreamReadError::UnexpectedEof("unexpected EOF reading merge-tree OID".into())
    })?;
    let tree = String::from_utf8_lossy(&tree_bytes).trim().to_string();
    if !is_object_name(&tree) {
        return Err(StreamReadError::Malformed(format!(
            "invalid merge-tree OID {tree:?}"
        )));
    }
    let mut named = Vec::new();
    loop {
        let field = read_nul_field(live, deadline)?.ok_or_else(|| {
            StreamReadError::UnexpectedEof("unexpected EOF reading merge-tree paths".into())
        })?;
        if field.is_empty() {
            break;
        }
        if clean {
            return Err(StreamReadError::Malformed(
                "clean merge-tree record carried unexpected path entries".into(),
            ));
        }
        named.push(render_path(&field));
    }
    if clean {
        Ok(MergeOutcome::Clean { tree })
    } else {
        if named.is_empty() {
            return Err(StreamReadError::Malformed(
                "git merge-tree reported a conflict and named no path".into(),
            ));
        }
        let paths_total = named.len();
        Ok(MergeOutcome::Conflicted {
            tree,
            paths: named.into_iter().take(MAX_CONFLICT_PATHS).collect(),
            paths_total,
        })
    }
}

/// Parses a `--messages` conflict-group path count strictly as a bounded
/// `usize` (`<= MAX_MESSAGE_PATH_COUNT`), rejecting negative numbers, signs,
/// whitespace, non-digits, and counts above 1024.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_merge_tree_message_count(field: &[u8]) -> Result<usize, QuarantineError> {
    if field.is_empty() || !field.iter().all(u8::is_ascii_digit) {
        return Err(QuarantineError::Git(
            format!(
                "invalid merge-tree message path count: {:?}",
                String::from_utf8_lossy(field)
            )
            .into(),
        ));
    }
    let text = std::str::from_utf8(field).map_err(|_| {
        QuarantineError::Git("invalid UTF-8 in merge-tree message path count".into())
    })?;
    let count = text.parse::<usize>().map_err(|_| {
        QuarantineError::Git(format!("merge-tree message path count overflow: {text}").into())
    })?;
    if count > MAX_MESSAGE_PATH_COUNT {
        return Err(QuarantineError::Git(
            format!("merge-tree message path count {count} exceeds limit {MAX_MESSAGE_PATH_COUNT}")
                .into(),
        ));
    }
    Ok(count)
}

/// Parses `git merge-tree --write-tree --name-only --no-messages -z`.
///
/// Shared by the quarantine (a tree that is thrown away) and by lane
/// integration (a tree written into the repository's own object database).
/// Exit 0 is a clean merge, exit 1 is a conflict, and a git that rejects the
/// option is [`QuarantineError::Unsupported`].
pub(crate) fn parse_merge_tree_output(
    stdout: &[u8],
    status_code: i32,
    stderr: &str,
) -> Result<MergeOutcome, QuarantineError> {
    let stderr = stderr.trim();
    // Bytes, not text: a path git names need not be UTF-8, and a lossy
    // conversion would make two different names the same string.
    let mut fields = stdout.split(|byte| *byte == 0);
    let tree = String::from_utf8_lossy(fields.next().unwrap_or_default());
    let tree = tree.trim();
    match status_code {
        0 | 1 if is_object_name(tree) => {
            if status_code == 0 {
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
                    "git merge-tree reported a conflict and named no path".into(),
                ));
            }
            let paths_total = named.len();
            Ok(MergeOutcome::Conflicted {
                tree: tree.to_string(),
                paths: named.into_iter().take(MAX_CONFLICT_PATHS).collect(),
                paths_total,
            })
        }
        _ => Err(classify_failure(status_code, stderr)),
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
        QuarantineError::Git(format!("git merge-tree exited {status_code}: {stderr}").into())
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
    .unwrap_or_else(|reason: CommandError| MergeCandidate::Unknown {
        reason: reason.into_message(),
    })
}

/// Err when `repo` is a partial clone, or when that could not be told: a merge
/// there reads file contents, and git would fetch the missing ones from the
/// remote. A caller that merges many pairs asks once and then opens a stream
/// with [`ObjectQuarantine::open_stream`].
pub(crate) fn refuse_partial_clone(repo: &Path) -> Result<(), CommandError> {
    match is_partial_clone(repo) {
        Ok(false) => Ok(()),
        Ok(true) => Err(
            "this repository is a partial clone: a merge reads file contents \
             and git would fetch missing ones from the remote; nothing was run"
                .into(),
        ),
        Err(reason) => Err(reason.wrap(|reason| {
            format!("could not tell whether this is a partial clone, so nothing was run: {reason}")
        })),
    }
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
fn resolve_commit(repo: &Path, operand: &str, deadline: Instant) -> Result<String, CommandError> {
    if operand.is_empty() || operand.starts_with('-') {
        return Err(format!("{operand:?} is not a commit name").into());
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return Err(QuarantineError::DeadlineExceeded.to_string().into());
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
        if is_run_timeout(e.message()) {
            QuarantineError::DeadlineExceeded.to_string()
        } else {
            format!(
                "could not resolve {operand:?} to a commit: {e}",
                e = e.message()
            )
        }
    })?;
    let oid = text.trim();
    if is_object_name(oid) {
        Ok(oid.to_string())
    } else {
        Err(format!("{operand:?} did not resolve to a commit").into())
    }
}

/// Whether the repository is a partial clone (`extensions.partialclone` or a
/// promisor remote). Read from configuration only, so it fetches nothing.
pub(crate) fn is_partial_clone(repo: &Path) -> Result<bool, CommandError> {
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
            )
            .into())
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
        )
        .into()),
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
        if !meta.is_dir() || !owned_by_this_user(&meta, &path) {
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
fn owned_by_this_user(meta: &std::fs::Metadata, _path: &Path) -> bool {
    // SAFETY: `geteuid` reads this process's own credentials and cannot fail.
    std::os::unix::fs::MetadataExt::uid(meta) == unsafe { libc::geteuid() }
}

/// By the directory's owner SID: this user's, or Administrators' for an
/// elevated run. An owner that cannot be read is not ours, so the sweep skips
/// it. This answered "yes" for anything until [`crate::win_private`] existed.
#[cfg(windows)]
fn owned_by_this_user(_meta: &std::fs::Metadata, path: &Path) -> bool {
    crate::win_private::sys::of_path(path).is_some_and(|security| security.owned_by_me())
}

#[cfg(not(any(unix, windows)))]
fn owned_by_this_user(_meta: &std::fs::Metadata, _path: &Path) -> bool {
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

#[cfg(test)]
mod stream_tests {
    use super::*;
    use crate::procguard::LockedSpawn;
    use std::process::Command;

    fn soon() -> Instant {
        Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(60))
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let mut cmd = Command::new("git");
        crate::test_support::pin_git(&mut cmd);
        let out = cmd
            .args(args)
            .current_dir(dir)
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn commit_file(dir: &Path, file: &str, content: &str) -> String {
        if let Some(parent) = dir.join(file).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(dir.join(file), content).unwrap();
        git(dir, &["add", "-A"]);
        git(dir, &["commit", "-q", "-m", file]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn twenty_pair_mixed_batch_completes_in_one_stream_with_at_most_two_respawns() {
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "T"]);
        git(repo, &["config", "user.email", "t@e.com"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(repo);

        commit_file(repo, "shared.txt", "root\n");
        let base = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["checkout", "-q", "-b", "ours-conflict"]);
        let ours_conflict = commit_file(repo, "shared.txt", "ours\n");
        git(repo, &["checkout", "-q", &base]);
        let mut clean_tips = Vec::new();
        for i in 0..15 {
            git(
                repo,
                &["checkout", "-q", "-b", &format!("clean-{i}"), &base],
            );
            clean_tips.push(commit_file(
                repo,
                &format!("clean_{i}.txt"),
                &format!("c{i}\n"),
            ));
        }
        git(repo, &["checkout", "-q", "-b", "theirs-conflict", &base]);
        let theirs_conflict = commit_file(repo, "shared.txt", "theirs\n");
        git(repo, &["checkout", "-q", "--orphan", "island"]);
        git(repo, &["rm", "-rf", "-q", "."]);
        let orphan = commit_file(repo, "orphan.txt", "alone\n");
        git(repo, &["checkout", "-q", "main"]);

        // 20 mixed pairs: clean, conflict, 1 unrelated-history (rc=128), 1 bad OID (rc=1).
        let bad_oid = "deadbeef".repeat(5);
        let mut pairs: Vec<(&str, &str, &str)> = Vec::new();
        for tip in clean_tips.iter().take(10) {
            pairs.push((ours_conflict.as_str(), tip.as_str(), "clean"));
        }
        pairs.push((&ours_conflict, &theirs_conflict, "conflict"));
        pairs.push((&ours_conflict, &orphan, "unrelated"));
        for tip in clean_tips.iter().take(3) {
            pairs.push((ours_conflict.as_str(), tip.as_str(), "clean"));
        }
        pairs.push((&ours_conflict, &bad_oid, "bad_oid"));
        pairs.push((&ours_conflict, &theirs_conflict, "conflict"));
        for tip in clean_tips.iter().take(3) {
            pairs.push((ours_conflict.as_str(), tip.as_str(), "clean"));
        }
        assert_eq!(pairs.len(), 20);

        let mut stream = ObjectQuarantine::open_stream(repo, soon()).expect("open_stream");
        let q_dir = stream.quarantine_path().to_path_buf();
        assert!(q_dir.is_dir());
        for (idx, (a, b, kind)) in pairs.iter().enumerate() {
            let verdict = stream.merge_candidate(a, b);
            match *kind {
                "clean" => assert!(
                    matches!(verdict, MergeCandidate::Clean { .. }),
                    "pair {idx}: {verdict:?}"
                ),
                "conflict" => assert!(
                    matches!(verdict, MergeCandidate::Conflict { .. }),
                    "pair {idx}: {verdict:?}"
                ),
                "unrelated" | "bad_oid" => assert!(
                    matches!(&verdict, MergeCandidate::Unknown { reason } if !reason.is_empty()),
                    "pair {idx} ({kind}) must be Unknown, never Clean: {verdict:?}"
                ),
                other => unreachable!("{other}"),
            }
        }
        assert!(
            stream.respawn_count() <= 2,
            "expected <= 2 respawns, got {}",
            stream.respawn_count()
        );
        assert_eq!(stream.spawn_count(), 1 + stream.respawn_count());
        drop(stream);
        assert!(
            !q_dir.exists(),
            "quarantine {} survived stream drop",
            q_dir.display()
        );
    }

    #[test]
    fn parse_message_count_rejects_negative_and_oversized_counts() {
        assert!(parse_merge_tree_message_count(b"-3").is_err());
        assert!(parse_merge_tree_message_count(b"999999999").is_err());
        assert!(parse_merge_tree_message_count(b"1025").is_err());
        assert!(parse_merge_tree_message_count(b"+5").is_err());
        assert!(parse_merge_tree_message_count(b"").is_err());
        assert!(parse_merge_tree_message_count(b"abc").is_err());
        assert_eq!(parse_merge_tree_message_count(b"0").unwrap(), 0);
        assert_eq!(parse_merge_tree_message_count(b"1024").unwrap(), 1024);
    }

    #[cfg(unix)]
    #[test]
    fn exit_129_falls_back_to_single_shot() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().expect("tempdir");
        let repo = dir.path();
        git(repo, &["init", "-q", "-b", "main"]);
        git(repo, &["config", "user.name", "T"]);
        git(repo, &["config", "user.email", "t@e.com"]);
        git(repo, &["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(repo);
        let base = commit_file(repo, "root.txt", "root\n");
        git(repo, &["checkout", "-q", "-b", "feat"]);
        let feat = commit_file(repo, "f.txt", "f\n");
        git(repo, &["checkout", "-q", "main"]);
        let main = commit_file(repo, "m.txt", "m\n");
        let _ = base;

        let real_git = {
            let out = Command::new("sh")
                .args(["-c", "command -v git"])
                .output_locked()
                .unwrap();
            String::from_utf8(out.stdout).unwrap().trim().to_string()
        };
        let shim_dir = tempfile::tempdir().unwrap();
        let shim = shim_dir.path().join("git");
        let script = format!(
            "#!/bin/sh\nfor a in \"$@\"; do\n  if [ \"$a\" = \"--stdin\" ]; then\n    echo \"error: unknown option \\`stdin'\" >&2\n    echo \"usage: git merge-tree [<options>] <branch1> <branch2>\" >&2\n    exit 129\n  fi\ndone\nexec {real_git} \"$@\"\n"
        );
        std::fs::write(&shim, script).unwrap();
        std::fs::set_permissions(&shim, std::fs::Permissions::from_mode(0o755)).unwrap();
        let orig = std::env::var_os("PATH").unwrap_or_default();
        let new_path = std::env::join_paths(
            std::iter::once(shim_dir.path().to_path_buf()).chain(std::env::split_paths(&orig)),
        )
        .unwrap();

        crate::engine::git_cli::with_test_git_path(&new_path, || {
            let mut stream = ObjectQuarantine::open_stream(repo, soon()).expect("open_stream");
            let verdict = stream.merge_candidate(&main, &feat);
            assert!(
                matches!(verdict, MergeCandidate::Clean { .. }),
                "expected single-shot fallback clean, got {verdict:?}"
            );
            assert!(stream.single_shot_fallback);
        });
    }
}
