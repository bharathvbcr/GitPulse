//! Turn checkpoints: the files of a worktree as they stood at an agent turn
//! boundary, kept as a commit under a hidden ref so they can be listed and
//! diffed later. Capture, list and diff only: nothing here restores anything.
//!
//! A checkpoint is `commit-tree` over a tree built from a private copy of the
//! worktree's index (so Git's stat cache is reused and unchanged files are not
//! rehashed), with tracked changes staged by `add -u` and the admitted
//! untracked files staged by name. The commit's only parent is the `HEAD` it
//! was taken on.
//!
//! # Security-Impact Statement (`refs/gitpulse/turns/*`)
//!
//! - Namespace and authority: checkpoints live only under
//!   `refs/gitpulse/turns/<key>/<stamp>`, written by the desktop app process
//!   (`serve_turn_checkpoint`), never by the decide-only hook, which only sends
//!   `Request::RecordTurnCheckpoint` over the UID-authenticated single-instance
//!   mailbox and only when `turn_checkpoints.enabled` is set (off by default).
//!   The app does not trust the hook for that: `serve_turn_checkpoint` reads
//!   the setting itself and, when it is off or unreadable, answers `skipped`
//!   before any Git process, ref or ledger row, so a hook of another version
//!   (or any local process that can reach the mailbox) cannot make a worktree
//!   capture while the person has the feature off. Requests are also bounded:
//!   at most [`crate::desktop::instance`]'s `MAX_CHECKPOINT_HANDLERS` (2)
//!   captures are in flight at once, a share of the mailbox's gate slots, and
//!   a further one is answered busy, so a burst of turn boundaries across many
//!   worktrees cannot occupy the mailbox or run unbounded Git work.
//!   `<key>` hashes the branch ref name together with the worktree's own Git
//!   directory (or, for a detached `HEAD`, that directory alone), so two
//!   worktrees of one repository never read each other's checkpoints, even
//!   when a branch moves between them. Reads (`list`, `diff`) resolve ids only
//!   inside the caller's own key.
//! - What it never touches: the real index, `HEAD`, branch refs, the stash
//!   list, `.git/config` and every working file. The index used for staging is
//!   a private 0600 file removed on every exit path by an RAII guard;
//!   `core.splitIndex` and `core.fsmonitor` are forced off for the staging
//!   commands so no shared index or daemon state is written into the Git
//!   directory. The ref is created with an expected-absent old value, so a
//!   racing writer is never overwritten; pruning deletes with the expected old
//!   value.
//! - Secrets, fail closed: files Git already tracks are captured as they are
//!   in the worktree now, WITHOUT a credential scan: that includes uncommitted
//!   edits to tracked files and newly staged files, so a checkpoint can hold a
//!   secret the person typed into a tracked file and has not committed. It
//!   stays in the repository's own object store, under a ref no UI lists, and
//!   is bounded by retention (50 per worktree, 7 days). An untracked file is
//!   admitted only if its leaf name does not start with `.env` (so `.env`,
//!   `.env.local`, `.envrc`, `.env-local` and `.env_prod` are all omitted) and
//!   `dc_redact` (the one detector the commit-path leak scan and
//!   the ledger redactor read) finds no credential shape in its bytes. A file
//!   that cannot be read, is larger than the per-file scan limit, or is not a
//!   regular file or symlink (a FIFO is never opened) is omitted. If the scan
//!   errors or exceeds its time bound, or the candidates exceed the file or
//!   byte cap, NO untracked file is admitted. Every omission is counted in the
//!   result, the commit message and the ledger row, so a partial snapshot is
//!   never read as a whole one. A file changed between the scan and the
//!   staging is hashed as it then is: the scan is a gate on what was read, not
//!   a lock.
//! - Bounds: every Git call runs under the repository's Git runner (spawn
//!   gate, environment scrub, output cap, 90 s deadline); a capture that hits
//!   one is `failed`, never `captured`. List and diff output are capped and say
//!   when they were cut. The label is redacted, stripped of control characters
//!   and cut to [`MAX_LABEL_CHARS`]; the session id is reduced to a safe
//!   alphabet.
//! - Concurrency: a per-worktree capture guard, not the repository mutation
//!   lock, so a long capture never blocks a user's commit. A second request for
//!   a worktree whose capture is running is answered `coalesced`.

use std::collections::HashSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::engine::git_cli::{
    find_git_root, git_captured, git_captured_with_stdin, git_text_capped, git_with_index,
    validate_repo,
};
use crate::engine::git_writer::{
    absent_oid_for_len, validate_oid, validate_ref_name, SNAPSHOT_IDENTITY_ARGS,
};
use crate::engine::process::BoundedRun;

/// Ref prefix under which turn checkpoints are stored.
pub const TURN_REF_PREFIX: &str = "refs/gitpulse/turns/";
/// Checkpoints kept per worktree key (mirrors `git_writer::MAX_OPS`).
pub const MAX_TURN_CHECKPOINTS: usize = 50;
/// Checkpoint retention in seconds (mirrors `git_writer::OP_EXPIRY_SECS`).
pub const TURN_EXPIRY_SECS: u64 = 7 * 24 * 3600;
/// Untracked files one capture will consider. A worktree with more is almost
/// always a build output or dependency tree that is not ignored; hashing it
/// every turn would cost more than the checkpoint is worth.
pub const MAX_UNTRACKED_FILES: usize = 5_000;
/// Total size, by `stat`, of the untracked files one capture will consider.
pub const MAX_UNTRACKED_BYTES: u64 = 256 * 1024 * 1024;
/// The largest untracked file read for the secret scan. A larger one is
/// omitted rather than admitted unscanned.
pub const MAX_SCAN_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// Wall-clock bound on the secret scan of one capture.
pub const SCAN_BUDGET: Duration = Duration::from_secs(20);
/// Longest label kept, in characters.
pub const MAX_LABEL_CHARS: usize = 200;
/// Most checkpoints one list call returns.
pub const MAX_LIST_LIMIT: usize = 50;
/// Bytes of patch one diff call returns.
pub const MAX_DIFF_BYTES: usize = 256 * 1024;
/// Bytes of `--numstat` read per listed checkpoint.
const MAX_STAT_BYTES: usize = 256 * 1024;
const MAX_SESSION_CHARS: usize = 96;
const KEY_HEX_CHARS: usize = 16;
const STAMP_DIGITS: usize = 16;
/// Retries of the create-only ref when the stamp is already taken.
const REF_CREATE_ATTEMPTS: usize = 8;

/// Git options every staging command runs with: no shared index and no
/// fsmonitor daemon may be written or started on the checkpoint's behalf.
const STAGING_FLAGS: [&str; 4] = ["-c", "core.fsmonitor=false", "-c", "core.splitIndex=false"];

/// Which turn boundary a checkpoint was taken at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnEvent {
    TurnStart,
    TurnEnd,
}

impl TurnEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            TurnEvent::TurnStart => "turn_start",
            TurnEvent::TurnEnd => "turn_end",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "turn_start" => Some(TurnEvent::TurnStart),
            "turn_end" => Some(TurnEvent::TurnEnd),
            _ => None,
        }
    }
}

/// How one capture attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureOutcome {
    /// A new checkpoint ref was written.
    Captured,
    /// The newest checkpoint already has this tree and parent; nothing written.
    Unchanged,
    /// Another capture of this worktree was running; this request did nothing.
    Coalesced,
    /// The worktree is in a state that cannot be captured (reason stated).
    Skipped,
    /// A step failed or hit a bound (reason stated). Nothing was captured.
    Failed,
}

impl CaptureOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            CaptureOutcome::Captured => "captured",
            CaptureOutcome::Unchanged => "unchanged",
            CaptureOutcome::Coalesced => "coalesced",
            CaptureOutcome::Skipped => "skipped",
            CaptureOutcome::Failed => "failed",
        }
    }
}

/// What a checkpoint left out of the untracked files, and why. A checkpoint
/// with any omission is partial; a reader must be able to tell.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Omissions {
    /// Untracked, non-ignored paths Git listed.
    pub untracked_found: usize,
    /// Of those, how many are in the checkpoint's tree.
    pub untracked_admitted: usize,
    /// Files whose leaf name starts with `.env`.
    pub env_files: usize,
    /// Files in which the leak scanner found a credential shape.
    pub secret_flagged: usize,
    /// Files that were unreadable, vanished, or not a regular file or symlink.
    pub unreadable_or_special: usize,
    /// Files larger than [`MAX_SCAN_FILE_BYTES`]: never scanned, so never taken.
    pub too_large: usize,
    /// Nested repositories (listed by Git as a directory).
    pub nested_repos: usize,
    /// GitPulse's own `.devcouncil/` state.
    pub tool_state: usize,
    /// Set when NO untracked file was admitted: `cap_exceeded`, `scan_failed`,
    /// `scan_timeout` or `listing_failed`.
    pub wholesale: Option<String>,
    /// The numbers or error behind `wholesale`.
    pub wholesale_detail: Option<String>,
    pub cap_files: usize,
    pub cap_bytes: u64,
    /// Bytes, by `stat`, of the files considered (0 when never summed).
    pub found_bytes: u64,
}

impl Omissions {
    pub fn untracked_omitted(&self) -> usize {
        self.untracked_found.saturating_sub(self.untracked_admitted)
    }

    /// True when the checkpoint holds less than the whole worktree.
    pub fn is_partial(&self) -> bool {
        self.untracked_omitted() > 0 || self.wholesale.is_some()
    }

    fn trailer(&self) -> String {
        format!(
            "found={} admitted={} env={} flagged={} unreadable={} toolarge={} nested={} state={} wholesale={} cap_files={} cap_bytes={} found_bytes={}",
            self.untracked_found,
            self.untracked_admitted,
            self.env_files,
            self.secret_flagged,
            self.unreadable_or_special,
            self.too_large,
            self.nested_repos,
            self.tool_state,
            self.wholesale.as_deref().unwrap_or("-"),
            self.cap_files,
            self.cap_bytes,
            self.found_bytes,
        )
    }

    fn from_trailer(value: &str) -> Self {
        let mut out = Omissions::default();
        for token in value.split_whitespace() {
            let Some((key, val)) = token.split_once('=') else {
                continue;
            };
            let number = || val.parse::<u64>().unwrap_or(0);
            match key {
                "found" => out.untracked_found = number() as usize,
                "admitted" => out.untracked_admitted = number() as usize,
                "env" => out.env_files = number() as usize,
                "flagged" => out.secret_flagged = number() as usize,
                "unreadable" => out.unreadable_or_special = number() as usize,
                "toolarge" => out.too_large = number() as usize,
                "nested" => out.nested_repos = number() as usize,
                "state" => out.tool_state = number() as usize,
                "wholesale" if val != "-" && !val.is_empty() => {
                    out.wholesale = Some(val.chars().take(40).collect());
                }
                "cap_files" => out.cap_files = number() as usize,
                "cap_bytes" => out.cap_bytes = number(),
                "found_bytes" => out.found_bytes = number(),
                _ => {}
            }
        }
        out
    }
}

/// The answer to one capture attempt. Never an error: an attempt that could
/// not capture says `skipped` or `failed` with the reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureResult {
    pub outcome: CaptureOutcome,
    pub event: TurnEvent,
    pub reason: Option<String>,
    pub key: Option<String>,
    /// The stamp: the checkpoint's id within its worktree key.
    pub id: Option<String>,
    pub ref_name: Option<String>,
    pub commit: Option<String>,
    pub tree: Option<String>,
    pub head: Option<String>,
    pub omissions: Omissions,
    /// Checkpoints deleted by this capture's prune.
    pub pruned: usize,
    /// Why the prune could not finish; the capture itself still stands.
    pub prune_error: Option<String>,
}

impl CaptureResult {
    fn bare(outcome: CaptureOutcome, event: TurnEvent, reason: Option<String>) -> Self {
        CaptureResult {
            outcome,
            event,
            reason,
            key: None,
            id: None,
            ref_name: None,
            commit: None,
            tree: None,
            head: None,
            omissions: Omissions::default(),
            pruned: 0,
            prune_error: None,
        }
    }
}

/// What a capture is asked to record.
#[derive(Debug, Clone, Copy)]
pub struct CaptureRequest<'a> {
    pub event: TurnEvent,
    pub session_id: Option<&'a str>,
    pub label: Option<&'a str>,
}

/// Limits and seams of one capture. [`CaptureConfig::production`] is what the
/// app uses; tests take the clock and every limit as parameters.
#[derive(Clone)]
pub struct CaptureConfig {
    pub now_ms: u64,
    pub max_keep: usize,
    pub expiry_secs: u64,
    pub max_untracked_files: usize,
    pub max_untracked_bytes: u64,
    pub scan_budget: Duration,
    /// Where the private index is created; the user cache directory, else the
    /// system temporary directory, when absent.
    pub index_dir: Option<PathBuf>,
    /// `Ok(true)` when the bytes carry a credential shape; `Err` when the scan
    /// could not run.
    pub scanner: fn(&[u8]) -> Result<bool, String>,
    #[cfg(test)]
    pub inject_failure: Option<&'static str>,
}

impl CaptureConfig {
    pub fn production() -> Self {
        CaptureConfig {
            now_ms: crate::ledger::ids::now_millis(),
            max_keep: MAX_TURN_CHECKPOINTS,
            expiry_secs: TURN_EXPIRY_SECS,
            max_untracked_files: MAX_UNTRACKED_FILES,
            max_untracked_bytes: MAX_UNTRACKED_BYTES,
            scan_budget: SCAN_BUDGET,
            index_dir: None,
            scanner: scan_for_secrets,
            #[cfg(test)]
            inject_failure: None,
        }
    }
}

/// The repository's one credential detector, over raw bytes.
fn scan_for_secrets(bytes: &[u8]) -> Result<bool, String> {
    Ok(dc_redact::contains_secret(&String::from_utf8_lossy(bytes)))
}

enum Stop {
    Skip(String),
    Fail(String),
}

fn fail<T>(error: impl Into<String>) -> Result<T, Stop> {
    Err(Stop::Fail(error.into()))
}

impl From<String> for Stop {
    fn from(error: String) -> Self {
        Stop::Fail(error)
    }
}

/// Runs git in `repo` through the repository's runner, returning the finished
/// run whatever its exit status and refusing a truncated one.
fn run(repo: &Path, args: &[&str]) -> Result<BoundedRun, String> {
    git_captured(repo, args)
        .and_then(|run| run.require_complete("git"))
        .map_err(|e| e.message().to_string())
}

fn stderr_text(run: &BoundedRun) -> String {
    String::from_utf8_lossy(&run.stderr[..run.stderr.len().min(500)])
        .trim()
        .to_string()
}

fn run_ok(repo: &Path, args: &[&str]) -> Result<String, String> {
    let finished = run(repo, args)?;
    if !finished.success {
        return Err(format!(
            "git {} failed: {}",
            crate::engine::git_cli::subcommand(args),
            stderr_text(&finished)
        ));
    }
    Ok(String::from_utf8_lossy(&finished.stdout).trim().to_string())
}

// ── per-worktree capture guard ────────────────────────────────────────────────

fn running() -> &'static Mutex<HashSet<PathBuf>> {
    static RUNNING: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    RUNNING.get_or_init(|| Mutex::new(HashSet::new()))
}

/// Held for the length of one capture of one worktree.
struct CaptureGuard(PathBuf);

impl CaptureGuard {
    fn try_acquire(worktree: &Path) -> Option<Self> {
        let mut set = running()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        set.insert(worktree.to_path_buf())
            .then(|| CaptureGuard(worktree.to_path_buf()))
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        running()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.0);
    }
}

// ── private temporary index ───────────────────────────────────────────────────

/// A private index file, removed (with its lock sibling) however the capture
/// ends: normal return, early error, or a panic unwinding through it.
struct TempIndex {
    path: PathBuf,
}

impl TempIndex {
    fn create(dir: Option<&Path>) -> Result<Self, String> {
        let dir = match dir {
            Some(dir) => dir.to_path_buf(),
            None => default_index_dir(),
        };
        std::fs::create_dir_all(&dir)
            .map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
        static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let name = format!(
            "gitpulse-turn-index-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        let path = dir.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options
            .open(&path)
            .map_err(|e| format!("cannot create the private index {}: {e}", path.display()))?;
        Ok(TempIndex { path })
    }

    /// Seeds the file with the bytes of `source`, the worktree's real index.
    /// `Ok(false)` when there is no real index to copy.
    fn seed_from(&self, source: &Path) -> Result<bool, String> {
        let mut input = match std::fs::File::open(source) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(format!("cannot read the index {}: {e}", source.display())),
        };
        let mut output = std::fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&self.path)
            .map_err(|e| format!("cannot open the private index: {e}"))?;
        std::io::copy(&mut input, &mut output)
            .map_err(|e| format!("cannot copy the index: {e}"))?;
        // Git treats an index entry as racily clean, and compares its content
        // instead of trusting its cached stat, when the entry's cached mtime
        // is not older than the index file's own mtime. A copy stamped "now"
        // is newer than every entry, so no entry would look racy and each
        // would be trusted from its cached stat. An entry that was racily
        // clean against the original index (edited in the same tick it was
        // staged, at the same size) would then be trusted too, and `add -u`
        // would miss the edit. Carrying the source's mtime over keeps exactly
        // the entries that were racy in the original racy in the copy. If the
        // time cannot be carried, this is a seed failure and the caller falls
        // back to `read-tree HEAD`.
        let modified = input
            .metadata()
            .and_then(|meta| meta.modified())
            .map_err(|e| format!("cannot read the index modified time: {e}"))?;
        output
            .set_modified(modified)
            .map_err(|e| format!("cannot set the private index modified time: {e}"))?;
        Ok(true)
    }
}

impl Drop for TempIndex {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
        let mut lock = self.path.clone().into_os_string();
        lock.push(".lock");
        let _ = std::fs::remove_file(PathBuf::from(lock));
    }
}

#[cfg(test)]
fn default_index_dir() -> PathBuf {
    // A test must not write into the developer's real cache directory.
    std::env::temp_dir().join("gitpulse-turn-checkpoint-tests")
}

#[cfg(not(test))]
fn default_index_dir() -> PathBuf {
    let base = crate::tool_config::default_cache_dir().unwrap_or_else(std::env::temp_dir);
    let dir = base.join("turn-checkpoints");
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        let _ = std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir);
    }
    dir
}

// ── keys, stamps, refs ────────────────────────────────────────────────────────

/// The key of a worktree's line of work. Includes the worktree's own Git
/// directory, so a branch moved to another worktree starts a new line there.
pub fn worktree_key(branch: Option<&str>, git_dir: &Path) -> String {
    let mut hasher = Sha256::new();
    match branch {
        Some(branch) => {
            hasher.update(b"branch\0");
            hasher.update(branch.as_bytes());
        }
        None => hasher.update(b"detached"),
    }
    hasher.update(b"\0");
    hasher.update(git_dir.as_os_str().as_encoded_bytes());
    hasher
        .finalize()
        .iter()
        .take(KEY_HEX_CHARS / 2)
        .fold(String::new(), |mut out, byte| {
            use std::fmt::Write;
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn valid_key(key: &str) -> bool {
    key.len() == KEY_HEX_CHARS && key.bytes().all(|b| b.is_ascii_hexdigit())
}

fn stamp_text(stamp_ms: u64) -> String {
    format!("{stamp_ms:0width$}", width = STAMP_DIGITS)
}

fn valid_stamp(stamp: &str) -> bool {
    !stamp.is_empty() && stamp.len() <= 20 && stamp.bytes().all(|b| b.is_ascii_digit())
}

fn ref_for(key: &str, stamp: &str) -> String {
    format!("{TURN_REF_PREFIX}{key}/{stamp}")
}

#[derive(Debug, Clone)]
struct Tip {
    key: String,
    stamp_ms: u64,
    stamp: String,
    ref_name: String,
    oid: String,
}

/// Every well-formed checkpoint ref under `prefix`, oldest first. A ref that
/// does not parse as `<key>/<digits>` is not a checkpoint and is ignored.
fn list_tips(repo: &Path, prefix: &str) -> Result<Vec<Tip>, String> {
    let out = run_ok(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)%00%(objectname)",
            prefix,
        ],
    )?;
    let mut tips = Vec::new();
    for line in out.lines() {
        let Some((ref_name, oid)) = line.split_once('\0') else {
            continue;
        };
        let Some(rest) = ref_name.strip_prefix(TURN_REF_PREFIX) else {
            continue;
        };
        let Some((key, stamp)) = rest.split_once('/') else {
            continue;
        };
        if !valid_key(key) || !valid_stamp(stamp) || validate_oid(oid).is_err() {
            continue;
        }
        let Ok(stamp_ms) = stamp.parse::<u64>() else {
            continue;
        };
        tips.push(Tip {
            key: key.to_string(),
            stamp_ms,
            stamp: stamp.to_string(),
            ref_name: ref_name.to_string(),
            oid: oid.to_string(),
        });
    }
    tips.sort_by_key(|tip| (tip.key.clone(), tip.stamp_ms));
    Ok(tips)
}

/// The tree and first parent of a commit.
fn commit_tree_and_parent(repo: &Path, oid: &str) -> Result<(String, Option<String>), String> {
    let text = run_ok(repo, &["cat-file", "commit", oid])?;
    let mut tree = None;
    let mut parent = None;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("tree ") {
            tree = Some(value.to_string());
        } else if let Some(value) = line.strip_prefix("parent ") {
            parent.get_or_insert_with(|| value.to_string());
        }
    }
    let tree = tree.ok_or_else(|| format!("commit {oid} has no tree"))?;
    Ok((tree, parent))
}

// ── sanitizing ────────────────────────────────────────────────────────────────

/// A one-line label safe to put in a commit subject: secrets redacted,
/// control characters and runs of whitespace collapsed to one space, cut to
/// [`MAX_LABEL_CHARS`].
pub fn sanitize_label(label: &str) -> String {
    let redacted = crate::ledger::redact::text(label);
    let mut out = String::new();
    let mut pending_space = false;
    for ch in redacted.chars() {
        if ch.is_control() || ch.is_whitespace() {
            pending_space = !out.is_empty();
            continue;
        }
        if pending_space {
            out.push(' ');
            pending_space = false;
        }
        out.push(ch);
        if out.chars().count() >= MAX_LABEL_CHARS {
            break;
        }
    }
    out
}

/// A session id reduced to `[A-Za-z0-9._:-]`, other characters replaced by
/// `_`, cut to [`MAX_SESSION_CHARS`]. Empty becomes `none`.
pub fn sanitize_session(session: Option<&str>) -> String {
    let cleaned: String = session
        .unwrap_or("")
        .trim()
        .chars()
        .take(MAX_SESSION_CHARS)
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '-') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "none".to_string()
    } else {
        cleaned
    }
}

// ── untracked files ───────────────────────────────────────────────────────────

fn path_from_bytes(bytes: &[u8]) -> Option<PathBuf> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(PathBuf::from(std::ffi::OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))]
    {
        std::str::from_utf8(bytes).ok().map(PathBuf::from)
    }
}

enum Candidate {
    File(PathBuf),
    Symlink(PathBuf),
}

fn is_env_name(name: &[u8]) -> bool {
    name.starts_with(b".env")
}

/// Reads at most `limit` bytes of a regular file without ever blocking on a
/// special file: opened non-blocking and without following a symlink, and
/// re-checked as a regular file once open.
fn read_regular_bounded(path: &Path, limit: u64) -> std::io::Result<Option<Vec<u8>>> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    Ok((bytes.len() as u64 <= limit).then_some(bytes))
}

struct Untracked {
    admitted: Vec<Vec<u8>>,
    omissions: Omissions,
}

fn wholesale(mut omissions: Omissions, code: &str, detail: String) -> Untracked {
    omissions.untracked_admitted = 0;
    omissions.wholesale = Some(code.to_string());
    omissions.wholesale_detail = Some(detail);
    Untracked {
        admitted: Vec::new(),
        omissions,
    }
}

/// Decides which untracked files may enter the checkpoint. Every path that is
/// not admitted is counted under the reason it was left out.
fn admit_untracked(repo: &Path, index: &Path, cfg: &CaptureConfig) -> Untracked {
    let mut om = Omissions {
        cap_files: cfg.max_untracked_files,
        cap_bytes: cfg.max_untracked_bytes,
        ..Omissions::default()
    };
    let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
    args.extend(["ls-files", "--others", "--exclude-standard", "-z"]);
    let listing = match git_with_index(repo, index, &args, b"") {
        Ok(bytes) => bytes,
        Err(e) => return wholesale(om, "listing_failed", e.message().to_string()),
    };
    let entries: Vec<&[u8]> = listing
        .split(|b| *b == 0)
        .filter(|entry| !entry.is_empty())
        .collect();
    om.untracked_found = entries.len();
    if entries.is_empty() {
        return Untracked {
            admitted: Vec::new(),
            omissions: om,
        };
    }
    if entries.len() > cfg.max_untracked_files {
        let detail = format!(
            "{} untracked files exceed the cap of {}",
            entries.len(),
            cfg.max_untracked_files
        );
        return wholesale(om, "cap_exceeded", detail);
    }

    let mut candidates: Vec<(Vec<u8>, Candidate)> = Vec::new();
    let mut total_bytes: u64 = 0;
    for entry in entries {
        if entry.ends_with(b"/") {
            om.nested_repos += 1;
            continue;
        }
        if entry.starts_with(b".devcouncil/") {
            om.tool_state += 1;
            continue;
        }
        let name = entry.rsplit(|b| *b == b'/').next().unwrap_or(entry);
        if is_env_name(name) {
            om.env_files += 1;
            continue;
        }
        let Some(relative) = path_from_bytes(entry) else {
            om.unreadable_or_special += 1;
            continue;
        };
        let full = repo.join(&relative);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            om.unreadable_or_special += 1;
            continue;
        };
        let kind = meta.file_type();
        if kind.is_symlink() {
            candidates.push((entry.to_vec(), Candidate::Symlink(full)));
        } else if kind.is_file() {
            total_bytes = total_bytes.saturating_add(meta.len());
            if total_bytes > cfg.max_untracked_bytes {
                om.found_bytes = total_bytes;
                let detail = format!(
                    "{} untracked files hold at least {total_bytes} bytes; the cap is {}",
                    om.untracked_found, cfg.max_untracked_bytes
                );
                return wholesale(om, "cap_exceeded", detail);
            }
            candidates.push((entry.to_vec(), Candidate::File(full)));
        } else {
            om.unreadable_or_special += 1;
        }
    }
    om.found_bytes = total_bytes;

    let started = Instant::now();
    let mut admitted = Vec::new();
    for (entry, candidate) in candidates {
        if started.elapsed() > cfg.scan_budget {
            let detail = format!("the scan passed its {:?} bound", cfg.scan_budget);
            return wholesale(om, "scan_timeout", detail);
        }
        let read = match &candidate {
            Candidate::Symlink(path) => std::fs::read_link(path)
                .map(|target| Some(target.to_string_lossy().into_owned().into_bytes())),
            Candidate::File(path) => read_regular_bounded(path, MAX_SCAN_FILE_BYTES),
        };
        let bytes = match read {
            Ok(Some(bytes)) => bytes,
            Ok(None) => {
                om.too_large += 1;
                continue;
            }
            Err(_) => {
                om.unreadable_or_special += 1;
                continue;
            }
        };
        match (cfg.scanner)(&bytes) {
            Ok(false) => admitted.push(entry),
            Ok(true) => om.secret_flagged += 1,
            Err(e) => return wholesale(om, "scan_failed", e),
        }
    }
    om.untracked_admitted = admitted.len();
    Untracked {
        admitted,
        omissions: om,
    }
}

/// Stages `paths` (literal, NUL-separated on stdin) into the private index.
/// A path that vanished between the scan and here is dropped and the rest
/// retried once: an agent's temporary file must not fail a whole capture.
fn stage_untracked(
    repo: &Path,
    index: &Path,
    paths: &mut Vec<Vec<u8>>,
    omissions: &mut Omissions,
) -> Result<(), String> {
    if paths.is_empty() {
        return Ok(());
    }
    for attempt in 0..2 {
        let mut stdin = Vec::new();
        for path in paths.iter() {
            stdin.extend_from_slice(path);
            stdin.push(0);
        }
        let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
        args.extend([
            "--literal-pathspecs",
            "add",
            "--pathspec-from-file=-",
            "--pathspec-file-nul",
        ]);
        match git_with_index(repo, index, &args, &stdin) {
            Ok(_) => return Ok(()),
            Err(e) if attempt == 0 => {
                let before = paths.len();
                paths.retain(|path| {
                    path_from_bytes(path)
                        .is_some_and(|p| std::fs::symlink_metadata(repo.join(p)).is_ok())
                });
                let dropped = before - paths.len();
                if dropped == 0 {
                    return Err(e.message().to_string());
                }
                omissions.unreadable_or_special += dropped;
                omissions.untracked_admitted = paths.len();
            }
            Err(e) => return Err(e.message().to_string()),
        }
    }
    Err("staging untracked files did not settle".into())
}

// ── capture ───────────────────────────────────────────────────────────────────

fn resolve_head(repo: &Path) -> Result<Option<String>, String> {
    let finished = run(repo, &["rev-parse", "--verify", "-q", "HEAD^{commit}"])?;
    if finished.success {
        let oid = String::from_utf8_lossy(&finished.stdout).trim().to_string();
        validate_oid(&oid).map_err(|e| e.message().to_string())?;
        return Ok(Some(oid));
    }
    if finished.status_code == 1 && finished.stderr.iter().all(u8::is_ascii_whitespace) {
        return Ok(None);
    }
    Err(format!("cannot resolve HEAD: {}", stderr_text(&finished)))
}

fn resolve_branch(repo: &Path) -> Result<Option<String>, String> {
    let finished = run(repo, &["symbolic-ref", "-q", "HEAD"])?;
    if finished.success {
        let name = String::from_utf8_lossy(&finished.stdout).trim().to_string();
        return Ok((!name.is_empty()).then_some(name));
    }
    if finished.status_code == 1 {
        return Ok(None);
    }
    Err(format!("cannot read HEAD: {}", stderr_text(&finished)))
}

/// Identity of this worktree's line of work: its key, branch and Git dir.
struct Line {
    key: String,
    branch: Option<String>,
}

fn resolve_line(repo: &Path) -> Result<Line, String> {
    let branch = resolve_branch(repo)?;
    let git_dir = run_ok(repo, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = std::fs::canonicalize(&git_dir).unwrap_or_else(|_| PathBuf::from(git_dir));
    Ok(Line {
        key: worktree_key(branch.as_deref(), &git_dir),
        branch,
    })
}

/// Takes one checkpoint of the worktree at `repo_path`. Never panics and
/// never errors: the outcome says what happened.
pub fn capture_turn_checkpoint(
    repo_path: &str,
    request: &CaptureRequest<'_>,
    cfg: &CaptureConfig,
) -> CaptureResult {
    let repo = match validate_repo(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            return CaptureResult::bare(
                CaptureOutcome::Failed,
                request.event,
                Some(e.message().to_string()),
            )
        }
    };
    let Some(_guard) = CaptureGuard::try_acquire(&repo) else {
        return CaptureResult::bare(
            CaptureOutcome::Coalesced,
            request.event,
            Some("a capture of this worktree is already running".into()),
        );
    };
    match capture_inner(&repo, request, cfg) {
        Ok(result) => result,
        Err(Stop::Skip(reason)) => {
            CaptureResult::bare(CaptureOutcome::Skipped, request.event, Some(reason))
        }
        Err(Stop::Fail(reason)) => {
            CaptureResult::bare(CaptureOutcome::Failed, request.event, Some(reason))
        }
    }
}

fn capture_inner(
    repo: &Path,
    request: &CaptureRequest<'_>,
    cfg: &CaptureConfig,
) -> Result<CaptureResult, Stop> {
    let Some(head) = resolve_head(repo)? else {
        return Err(Stop::Skip(
            "no commit yet: the branch is unborn, so there is nothing to take a checkpoint on"
                .into(),
        ));
    };
    let line = resolve_line(repo)?;

    let temp = TempIndex::create(cfg.index_dir.as_deref())?;
    let real_index = run_ok(
        repo,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?;
    // An index that is absent or cannot be copied is seeded from HEAD instead:
    // never an empty index, which would read as "every tracked file deleted".
    if !matches!(temp.seed_from(Path::new(&real_index)), Ok(true)) {
        let _ = std::fs::write(&temp.path, b"");
        let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
        args.extend(["read-tree", "HEAD"]);
        git_with_index(repo, &temp.path, &args, b"")
            .map_err(|e| Stop::Fail(e.message().to_string()))?;
    }

    // A parked merge, rebase or cherry-pick leaves unmerged entries, which
    // `add -u` would resolve by staging the working file. Look first.
    let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
    args.extend(["ls-files", "--unmerged", "-z"]);
    let unmerged = git_with_index(repo, &temp.path, &args, b"")
        .map_err(|e| Stop::Fail(e.message().to_string()))?;
    if !unmerged.is_empty() {
        return Err(Stop::Skip(
            "the index holds unmerged entries: a merge or rebase is parked".into(),
        ));
    }

    let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
    args.extend(["add", "-u"]);
    git_with_index(repo, &temp.path, &args, b"")
        .map_err(|e| Stop::Fail(e.message().to_string()))?;

    #[cfg(test)]
    if cfg.inject_failure == Some("after_tracked") {
        return fail("injected failure after staging tracked changes");
    }

    let mut untracked = admit_untracked(repo, &temp.path, cfg);
    stage_untracked(
        repo,
        &temp.path,
        &mut untracked.admitted,
        &mut untracked.omissions,
    )?;
    let omissions = untracked.omissions;

    let mut args: Vec<&str> = STAGING_FLAGS.to_vec();
    args.push("write-tree");
    let tree_out = git_with_index(repo, &temp.path, &args, b"")
        .map_err(|e| Stop::Fail(e.message().to_string()))?;
    let tree = String::from_utf8_lossy(&tree_out).trim().to_string();
    validate_oid(&tree).map_err(|e| Stop::Fail(e.message().to_string()))?;
    drop(temp);

    let own_prefix = format!("{TURN_REF_PREFIX}{}/", line.key);
    let tips = list_tips(repo, &own_prefix)?;
    if let Some(newest) = tips.last() {
        let (newest_tree, newest_parent) = commit_tree_and_parent(repo, &newest.oid)?;
        if newest_tree == tree && newest_parent.as_deref() == Some(head.as_str()) {
            let mut result = CaptureResult::bare(CaptureOutcome::Unchanged, request.event, None);
            result.reason = Some("the newest checkpoint already holds this tree".into());
            result.key = Some(line.key);
            result.id = Some(newest.stamp.clone());
            result.ref_name = Some(newest.ref_name.clone());
            result.commit = Some(newest.oid.clone());
            result.tree = Some(tree);
            result.head = Some(head);
            result.omissions = omissions;
            return Ok(result);
        }
    }

    let label = request.label.map(sanitize_label).unwrap_or_default();
    let session = sanitize_session(request.session_id);
    let mut message = format!("turn checkpoint ({})", request.event.as_str());
    if !label.is_empty() {
        message.push_str(": ");
        message.push_str(&label);
    }
    message.push_str(&format!(
        "\n\nGitPulse-Checkpoint: turn\nGitPulse-Event: {}\nGitPulse-Session: {session}\nGitPulse-Branch: {}\nGitPulse-Untracked: {}\n",
        request.event.as_str(),
        line.branch.as_deref().unwrap_or("detached"),
        omissions.trailer(),
    ));

    let mut commit_args: Vec<&str> = SNAPSHOT_IDENTITY_ARGS.to_vec();
    commit_args.extend(["commit-tree", &tree, "-p", &head, "-F", "-"]);
    let commit_run = git_captured_with_stdin(repo, &commit_args, message.as_bytes())
        .map_err(|e| Stop::Fail(e.message().to_string()))?;
    if !commit_run.success {
        return fail(format!(
            "git commit-tree failed: {}",
            stderr_text(&commit_run)
        ));
    }
    let commit = String::from_utf8_lossy(&commit_run.stdout)
        .trim()
        .to_string();
    validate_oid(&commit).map_err(|e| Stop::Fail(e.message().to_string()))?;

    // Stamps rise strictly within a key, so ids sort by age even if the clock
    // stepped back, and the create-only ref below cannot collide with our own.
    let floor = tips.last().map_or(0, |tip| tip.stamp_ms.saturating_add(1));
    let mut stamp_ms = cfg.now_ms.max(floor);
    let absent = absent_oid_for_len(head.len());
    let mut created = None;
    let mut last_error = String::new();
    for _ in 0..REF_CREATE_ATTEMPTS {
        let stamp = stamp_text(stamp_ms);
        let ref_name = ref_for(&line.key, &stamp);
        validate_ref_name(&ref_name).map_err(|e| Stop::Fail(e.message().to_string()))?;
        match create_ref_exclusive(repo, &ref_name, &commit, absent) {
            Ok(()) => {
                created = Some((stamp, ref_name));
                break;
            }
            Err(e) => {
                last_error = e;
                stamp_ms = stamp_ms.saturating_add(1);
            }
        }
    }
    let Some((stamp, ref_name)) = created else {
        return fail(format!(
            "could not create a checkpoint ref after {REF_CREATE_ATTEMPTS} attempts: {last_error}"
        ));
    };

    let (pruned, prune_error) = match prune(repo, &line.key, &ref_name, cfg) {
        Ok(count) => (count, None),
        Err(e) => (0, Some(e)),
    };

    let mut result = CaptureResult::bare(CaptureOutcome::Captured, request.event, None);
    result.key = Some(line.key);
    result.id = Some(stamp);
    result.ref_name = Some(ref_name);
    result.commit = Some(commit);
    result.tree = Some(tree);
    result.head = Some(head);
    result.omissions = omissions;
    result.pruned = pruned;
    result.prune_error = prune_error;
    Ok(result)
}

/// Creates `ref_name` at `oid` only if it does not exist (`absent` is Git's
/// all-zero expected old value), so a racing writer is never overwritten.
fn create_ref_exclusive(
    repo: &Path,
    ref_name: &str,
    oid: &str,
    absent: &str,
) -> Result<(), String> {
    let finished = run(repo, &["update-ref", ref_name, oid, absent])?;
    if finished.success {
        Ok(())
    } else {
        Err(stderr_text(&finished))
    }
}

/// Deletes checkpoints beyond `cfg.max_keep` for `key` (oldest first) and any
/// checkpoint of any key older than `cfg.expiry_secs`, never `keep_ref`.
/// Returns how many were deleted, or the first failure.
fn prune(repo: &Path, key: &str, keep_ref: &str, cfg: &CaptureConfig) -> Result<usize, String> {
    let tips = list_tips(repo, TURN_REF_PREFIX)?;
    let now_secs = cfg.now_ms / 1000;
    let mut removed = 0usize;
    let mut survivors: Vec<&Tip> = Vec::new();
    for tip in &tips {
        let expired = tip.ref_name != keep_ref
            && now_secs > tip.stamp_ms / 1000
            && now_secs - tip.stamp_ms / 1000 > cfg.expiry_secs;
        if expired {
            delete_tip(repo, tip)?;
            removed += 1;
        } else if tip.key == key {
            survivors.push(tip);
        }
    }
    let keep = cfg.max_keep.max(1);
    if survivors.len() > keep {
        let excess = survivors.len() - keep;
        for tip in survivors
            .into_iter()
            .filter(|tip| tip.ref_name != keep_ref)
            .take(excess)
        {
            delete_tip(repo, tip)?;
            removed += 1;
        }
    }
    Ok(removed)
}

fn delete_tip(repo: &Path, tip: &Tip) -> Result<(), String> {
    let finished = run(repo, &["update-ref", "-d", &tip.ref_name, &tip.oid])?;
    if finished.success {
        Ok(())
    } else {
        Err(format!(
            "could not delete {}: {}",
            tip.ref_name,
            stderr_text(&finished)
        ))
    }
}

// ── list ──────────────────────────────────────────────────────────────────────

/// One checkpoint, as listed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCheckpointSummary {
    /// The stamp; what `diff` takes.
    pub id: String,
    pub ref_name: String,
    pub commit: String,
    pub tree: String,
    /// The `HEAD` commit it was taken on.
    pub parent: Option<String>,
    /// Seconds since the epoch, from the stamp.
    pub created_at: i64,
    pub label: String,
    pub event: Option<String>,
    pub session_id: Option<String>,
    pub branch: Option<String>,
    /// Files changed against the previous checkpoint (the first, against its
    /// parent commit).
    pub files: usize,
    pub additions: u64,
    pub deletions: u64,
    /// False when the numstat was cut at its byte bound.
    pub stats_complete: bool,
    pub omissions: Omissions,
}

/// A worktree's checkpoints, newest first.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCheckpointList {
    pub key: String,
    pub entries: Vec<TurnCheckpointSummary>,
    /// All checkpoints this worktree key holds.
    pub total: usize,
    /// True when `entries` is shorter than `total`.
    pub truncated: bool,
}

struct Parsed {
    label: String,
    event: Option<String>,
    session_id: Option<String>,
    branch: Option<String>,
    omissions: Omissions,
}

fn parse_message(contents: &str) -> Parsed {
    let mut lines = contents.lines();
    let subject = lines.next().unwrap_or("");
    let label = subject
        .split_once("): ")
        .map(|(_, label)| label.to_string())
        .unwrap_or_default();
    let mut parsed = Parsed {
        label: label.chars().take(MAX_LABEL_CHARS).collect(),
        event: None,
        session_id: None,
        branch: None,
        omissions: Omissions::default(),
    };
    let mut in_body = false;
    for line in lines {
        if line.is_empty() {
            in_body = true;
            continue;
        }
        if !in_body {
            continue;
        }
        let Some((name, value)) = line.split_once(": ") else {
            continue;
        };
        let value = value.trim();
        match name {
            "GitPulse-Event" if parsed.event.is_none() => {
                parsed.event = TurnEvent::parse(value).map(|e| e.as_str().to_string());
            }
            "GitPulse-Session" if parsed.session_id.is_none() => {
                parsed.session_id = Some(value.chars().take(MAX_SESSION_CHARS).collect());
            }
            "GitPulse-Branch" if parsed.branch.is_none() => {
                parsed.branch = Some(value.chars().take(256).collect());
            }
            "GitPulse-Untracked" => parsed.omissions = Omissions::from_trailer(value),
            _ => {}
        }
    }
    parsed
}

/// `(files, additions, deletions, complete)` of `git diff --numstat base head`.
fn numstat(repo: &Path, base: &str, to: &str) -> Result<(usize, u64, u64, bool), String> {
    let (text, incomplete) = git_text_capped(
        repo,
        &[
            "diff",
            "--numstat",
            "--no-renames",
            "--no-ext-diff",
            "--no-textconv",
            base,
            to,
            "--",
        ],
        MAX_STAT_BYTES,
    )
    .map_err(|e| e.message().to_string())?;
    let mut files = 0usize;
    let mut additions = 0u64;
    let mut deletions = 0u64;
    for line in text.lines() {
        let mut fields = line.splitn(3, '\t');
        let (Some(add), Some(del), Some(_)) = (fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        files += 1;
        additions += add.parse::<u64>().unwrap_or(0);
        deletions += del.parse::<u64>().unwrap_or(0);
    }
    Ok((files, additions, deletions, incomplete.is_none()))
}

/// Lists up to `limit` of this worktree's checkpoints, newest first.
pub fn list_turn_checkpoints(repo_path: &str, limit: usize) -> Result<TurnCheckpointList, String> {
    let repo = validate_repo(repo_path).map_err(|e| e.message().to_string())?;
    let line = resolve_line(&repo)?;
    let prefix = format!("{TURN_REF_PREFIX}{}/", line.key);
    let tips = list_tips(&repo, &prefix)?;
    let total = tips.len();
    let limit = limit.clamp(1, MAX_LIST_LIMIT);
    // Newest first; one more than `limit` so the last listed has a baseline.
    let window: Vec<&Tip> = tips.iter().rev().take(limit + 1).collect();
    let mut entries = Vec::new();
    for (position, tip) in window.iter().take(limit).enumerate() {
        let contents = run_ok(&repo, &["cat-file", "commit", &tip.oid])?;
        let body = contents.split_once("\n\n").map_or("", |(_, body)| body);
        let parsed = parse_message(body);
        let (tree, parent) = commit_tree_and_parent(&repo, &tip.oid)?;
        let base = match window.get(position + 1) {
            Some(previous) => previous.oid.clone(),
            None => match &parent {
                Some(parent) => parent.clone(),
                None => run_ok(&repo, &["hash-object", "-t", "tree", "--stdin"])
                    .map_err(|_| "a checkpoint has no parent to compare with".to_string())?,
            },
        };
        let (files, additions, deletions, stats_complete) =
            numstat(&repo, &base, &tip.oid).unwrap_or((0, 0, 0, false));
        entries.push(TurnCheckpointSummary {
            id: tip.stamp.clone(),
            ref_name: tip.ref_name.clone(),
            commit: tip.oid.clone(),
            tree,
            parent,
            created_at: i64::try_from(tip.stamp_ms / 1000).unwrap_or(i64::MAX),
            label: parsed.label,
            event: parsed.event,
            session_id: parsed.session_id,
            branch: parsed.branch,
            files,
            additions,
            deletions,
            stats_complete,
            omissions: parsed.omissions,
        });
    }
    Ok(TurnCheckpointList {
        key: line.key,
        truncated: entries.len() < total,
        entries,
        total,
    })
}

// ── diff ──────────────────────────────────────────────────────────────────────

/// The patch of one checkpoint.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnCheckpointDiff {
    pub id: String,
    /// The checkpoint compared against, or `None` for its parent commit.
    pub against: Option<String>,
    pub base_commit: String,
    pub commit: String,
    pub patch: String,
    /// True when the patch was cut at [`MAX_DIFF_BYTES`].
    pub truncated: bool,
    /// True when a credential shape in the patch was redacted.
    pub redacted: bool,
}

fn resolve_checkpoint(repo: &Path, key: &str, id: &str) -> Result<String, String> {
    if !valid_stamp(id) {
        return Err("a checkpoint id is the digits of its stamp".into());
    }
    let stamp = stamp_text(id.parse::<u64>().map_err(|_| "the id is not a number")?);
    let ref_name = ref_for(key, &stamp);
    validate_ref_name(&ref_name).map_err(|e| e.message().to_string())?;
    let finished = run(
        repo,
        &[
            "rev-parse",
            "--verify",
            "-q",
            &format!("{ref_name}^{{commit}}"),
        ],
    )?;
    if !finished.success {
        return Err(format!("this worktree has no checkpoint {id}"));
    }
    let oid = String::from_utf8_lossy(&finished.stdout).trim().to_string();
    validate_oid(&oid).map_err(|e| e.message().to_string())?;
    Ok(oid)
}

/// The patch from `against` (or the checkpoint's parent commit) to the
/// checkpoint `id`, bounded, with external diff drivers and textconv off.
pub fn diff_turn_checkpoint(
    repo_path: &str,
    id: &str,
    against: Option<&str>,
) -> Result<TurnCheckpointDiff, String> {
    let repo = validate_repo(repo_path).map_err(|e| e.message().to_string())?;
    let line = resolve_line(&repo)?;
    let commit = resolve_checkpoint(&repo, &line.key, id)?;
    let base = match against {
        Some(other) => resolve_checkpoint(&repo, &line.key, other)?,
        None => commit_tree_and_parent(&repo, &commit)?
            .1
            .ok_or("the checkpoint has no parent commit to compare with")?,
    };
    let (patch, incomplete) = git_text_capped(
        &repo,
        &[
            "diff",
            "--no-ext-diff",
            "--no-textconv",
            "--no-color",
            "--no-renames",
            &base,
            &commit,
            "--",
        ],
        MAX_DIFF_BYTES,
    )
    .map_err(|e| e.message().to_string())?;
    let redacted_patch = dc_redact::redact_secrets(&patch);
    Ok(TurnCheckpointDiff {
        id: id.to_string(),
        against: against.map(str::to_string),
        base_commit: base,
        commit,
        redacted: redacted_patch != patch,
        patch: redacted_patch,
        truncated: incomplete.is_some(),
    })
}

// ── the app-side handler ──────────────────────────────────────────────────────

/// Handles [`crate::desktop::instance::Request::RecordTurnCheckpoint`] inside
/// the primary desktop process. The app owns the decision, not the hook: with
/// `turn_checkpoints.enabled` off or unreadable the answer is `skipped`, and no
/// git process runs, no ref is written and no ledger row is appended (the row
/// needs the repository's address, which reads Git). Otherwise every attempt
/// writes one ledger row naming its outcome; the reply carries the same result.
pub fn serve_turn_checkpoint(
    request: &crate::desktop::instance::Request,
) -> Result<serde_json::Value, String> {
    serve_turn_checkpoint_with(request, &|| {
        crate::tool_config::turn_checkpoint_settings().enabled
    })
}

/// [`serve_turn_checkpoint`] with where the setting is read from passed in.
pub(crate) fn serve_turn_checkpoint_with(
    request: &crate::desktop::instance::Request,
    enabled: &dyn Fn() -> bool,
) -> Result<serde_json::Value, String> {
    let crate::desktop::instance::Request::RecordTurnCheckpoint {
        cwd,
        session_id,
        event,
        label,
        ..
    } = request
    else {
        return Err("not a turn-checkpoint request".into());
    };
    let event = TurnEvent::parse(event).ok_or("unknown turn checkpoint event")?;
    if !enabled() {
        return serde_json::to_value(CaptureResult::bare(
            CaptureOutcome::Skipped,
            event,
            Some("turn checkpoints are off: turn_checkpoints.enabled is not set".into()),
        ))
        .map_err(|e| e.to_string());
    }
    let root = find_git_root(Path::new(cwd))
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| cwd.clone());
    let cfg = CaptureConfig::production();
    let result = capture_turn_checkpoint(
        &root,
        &CaptureRequest {
            event,
            session_id: session_id.as_deref(),
            label: label.as_deref(),
        },
        &cfg,
    );
    crate::ledger::record_turn_checkpoint_event(
        &root,
        session_id.as_deref(),
        result.event.as_str(),
        result.outcome.as_str(),
        result.reason.as_deref(),
        result.ref_name.as_deref(),
        result.head.as_deref(),
        &serde_json::json!({
            "untracked_found": result.omissions.untracked_found,
            "untracked_admitted": result.omissions.untracked_admitted,
            "untracked_omitted": result.omissions.untracked_omitted(),
            "env_files": result.omissions.env_files,
            "secret_flagged": result.omissions.secret_flagged,
            "unreadable_or_special": result.omissions.unreadable_or_special,
            "too_large": result.omissions.too_large,
            "nested_repos": result.omissions.nested_repos,
            "tool_state": result.omissions.tool_state,
            "wholesale": result.omissions.wholesale,
            "wholesale_detail": result.omissions.wholesale_detail,
            "cap_files": result.omissions.cap_files,
            "cap_bytes": result.omissions.cap_bytes,
            "found_bytes": result.omissions.found_bytes,
            "pruned": result.pruned,
            "prune_error": result.prune_error,
        }),
        cfg.now_ms,
    );
    serde_json::to_value(&result).map_err(|e| e.to_string())
}

#[cfg(test)]
#[path = "turn_checkpoint_tests.rs"]
mod tests;
