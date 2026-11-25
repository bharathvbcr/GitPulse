//! `host-relief.v1.json`: what the pressure-relief supervisor is doing, as a
//! file any GitPulse process can read.
//!
//! # Why a file
//!
//! The supervisor (`relief`) lowers agent descendants to background priority
//! and pauses (SIGSTOPs) the heaviest build or test subtree under a
//! GitPulse-launched agent while memory is still getting worse. The people and
//! processes that must know about it are not the supervisor's thread: the
//! command-gate hook (which tells the paused agent not to retry), the menu-bar
//! glyph and the native banner (which must work when the web view is the
//! casualty), and Health. They read this file, beside `host-load.v1`, through
//! [`read_status`] and nothing else.
//!
//! # The contract the writer keeps
//!
//! * Every `*_ms` field is milliseconds since the Unix epoch (`since_ms`
//!   included), never a monotonic reading.
//! * `writer_pid` and `writer_start_us` identify the process that wrote the
//!   file (start time in microseconds since the epoch, as
//!   `sysload::process_start_us` reports it).
//! * `slowed` counts processes actually slowed, so it is 0 in Advise mode;
//!   `would_slow` is what Act would have slowed, and is 0 in Act mode.
//! * `frozen` lists every tree the supervisor stopped, plus, in Advise mode,
//!   the tree it would have stopped with `advised_only: true`. Only the first
//!   kind is paused.
//! * `repo_path` is the root of the worktree or checkout the agent session
//!   runs in, not the main repository the worktree belongs to.
//! * The file is written with [`write_status`]: atomically, mode 0600.
//! * The supervisor refreshes it every tick while it claims activity, and
//!   writes it once more on the transition to rest.
//!
//! # Honesty
//!
//! A reader never turns "could not tell" into "nothing is paused".
//! [`ReliefOutcome`] has four states: [`Absent`](ReliefOutcome::Absent) (no
//! supervisor has ever written one), [`Fresh`](ReliefOutcome::Fresh),
//! [`Stale`](ReliefOutcome::Stale) and [`Unreadable`](ReliefOutcome::Unreadable).
//!
//! A file *claims activity* when its rung is not `none`, or any tree is really
//! paused, or `slowed` is above zero ([`ReliefStatus::claims_activity`]). One
//! that does not is at rest and its age says nothing. One that does is Stale
//! when it is older than [`STALE_AFTER_MS`], when it is dated more than
//! [`FUTURE_SLACK_MS`] ahead of the reader's clock, or when its writer (pid and
//! start time) is no longer running: a process it stopped may still be stopped.
//!
//! The file is opened as `host-load.v1` is (`fs_entry::open_private_record`):
//! no symlink, a regular file, owned by this user, private to them. Its
//! `command` text reaches an agent's context, so a file another user could have
//! written is Unreadable.
//!
//! # The resume seam
//!
//! The "Resume now" action of the banner and the Health button cannot call the
//! supervisor from another process. [`request_resume`] writes
//! `host-relief-resume.request` (`{reason, at_ms}`) atomically beside the status
//! file. The supervisor calls [`take_resume_request`] on each tick, which reads
//! and deletes it and returns nothing for one older than [`REQUEST_TTL_MS`]. It
//! then calls `relief::resume_all(true, reason)` and applies the request only to
//! pauses whose `since_ms` is at or before the request's `at_ms`: a request
//! never resumes a pause that began after it was made. Any process of the same
//! user can write the file, so the supervisor decides how far to trust one; it
//! can only end a pause, never start one.

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

/// The file's name, beside `host-load.v1`.
pub const STATUS_FILE_NAME: &str = "host-relief.v1.json";
/// The request a banner or Health button leaves for the supervisor.
pub const RESUME_REQUEST_FILE_NAME: &str = "host-relief-resume.request";
/// The version this build writes and the only one it reads.
pub const VERSION: u32 = 1;
/// A file that claims activity and is older than this is Stale.
pub const STALE_AFTER_MS: u64 = 30_000;
/// How far ahead of the reader's clock a file may be dated before it is Stale.
pub const FUTURE_SLACK_MS: u64 = 5_000;
/// A resume request older than this is dropped unread.
pub const REQUEST_TTL_MS: u64 = 2 * 60_000;
/// The most failures the file carries, newest last.
// Used by `write_status`, whose caller is the supervisor's writer.
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_FAILURES: usize = 16;
/// The most bytes read from either file; a larger one is Unreadable.
const MAX_BYTES: usize = 256 * 1024;
/// The longest reason a resume request carries.
const MAX_REASON: usize = 64;
/// How far a writer's recorded start may differ from the live process's.
const START_SLACK_US: u64 = 2_000_000;

/// What the supervisor is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliefMode {
    Off,
    Advise,
    Act,
}

/// How far the supervisor has gone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliefRung {
    None,
    Slow,
    Freeze,
}

/// The resource the verdict that justified the rung was bound by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliefBinding {
    Swap,
    CompressorSegments,
    CompressorThrash,
    MemoryPressure,
}

impl ReliefBinding {
    /// The resource in words a person reads.
    pub fn phrase(self) -> &'static str {
        match self {
            Self::Swap => "swap is nearly full",
            Self::CompressorSegments => "the memory compressor is nearly out of segments",
            Self::CompressorThrash => "the memory compressor is thrashing",
            Self::MemoryPressure => "memory pressure is critical",
        }
    }
}

/// One process tree the supervisor stopped, or would have stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenTree {
    pub pid: u32,
    pub start_us: u64,
    pub agent_root_pid: u32,
    pub terminal_session_id: Option<String>,
    /// The root of the worktree or checkout the agent session runs in.
    pub repo_path: String,
    pub command: String,
    pub footprint_bytes: u64,
    /// Unix epoch milliseconds.
    pub since_ms: u64,
    /// The mode was Advise: this is what Act would have paused, and nothing is
    /// stopped. Never counted as paused.
    pub advised_only: bool,
}

/// What an action did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliefActionKind {
    Slow,
    Freeze,
    Resume,
    CapResume,
    Refused,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliefAction {
    /// Unix epoch milliseconds.
    pub at_ms: u64,
    pub kind: ReliefActionKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliefFailure {
    /// Unix epoch milliseconds.
    pub at_ms: u64,
    pub pid: u32,
    pub what: String,
}

/// The published status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliefStatus {
    pub version: u32,
    /// Unix epoch milliseconds.
    pub written_at_ms: u64,
    /// The process that wrote this file.
    pub writer_pid: u32,
    /// That process's start time, microseconds since the Unix epoch.
    pub writer_start_us: u64,
    pub mode: ReliefMode,
    pub rung: ReliefRung,
    pub binding: Option<ReliefBinding>,
    /// Processes actually slowed; 0 in Advise mode.
    pub slowed: u32,
    /// Processes Act would have slowed; 0 in Act mode.
    pub would_slow: u32,
    pub frozen: Vec<FrozenTree>,
    pub last_action: Option<ReliefAction>,
    /// Bounded to [`MAX_FAILURES`], newest last.
    pub failures: Vec<ReliefFailure>,
    pub cap_hits: u32,
}

impl ReliefStatus {
    /// A status at rest: nothing slowed, nothing paused.
    pub fn at_rest(mode: ReliefMode, now_ms: u64) -> Self {
        Self {
            version: VERSION,
            written_at_ms: now_ms,
            writer_pid: std::process::id(),
            writer_start_us: 0,
            mode,
            rung: ReliefRung::None,
            binding: None,
            slowed: 0,
            would_slow: 0,
            frozen: Vec::new(),
            last_action: None,
            failures: Vec::new(),
            cap_hits: 0,
        }
    }

    /// The trees that are actually stopped. An advised-only entry is what
    /// Act would have done and is not one of them; every surface that says
    /// "paused" counts these and no others.
    pub fn paused(&self) -> impl Iterator<Item = &FrozenTree> {
        self.frozen.iter().filter(|tree| !tree.advised_only)
    }

    pub fn paused_count(&self) -> usize {
        self.paused().count()
    }

    /// Whether the file claims the supervisor is doing something, which is
    /// what makes its age and its writer matter: a rung above none, a tree
    /// really paused, or a process really slowed.
    pub fn claims_activity(&self) -> bool {
        self.rung != ReliefRung::None || self.paused_count() > 0 || self.slowed > 0
    }
}

/// Why a status that claims activity cannot be relied on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReliefStaleCause {
    /// Not refreshed within [`STALE_AFTER_MS`].
    Age,
    /// The process that wrote it is no longer running.
    WriterGone,
    /// Dated ahead of this machine's clock by more than [`FUTURE_SLACK_MS`].
    ClockAhead,
}

impl ReliefStaleCause {
    /// A kind that never changes with the age, for keying a notice on it.
    pub fn code(self) -> &'static str {
        match self {
            Self::Age => "age",
            Self::WriterGone => "writer_gone",
            Self::ClockAhead => "clock_ahead",
        }
    }
}

/// What a read found. Serialised for IPC as `{state, ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReliefOutcome {
    /// No file: no supervisor has written one here.
    Absent,
    /// Readable, and either at rest or refreshed within [`STALE_AFTER_MS`] by a
    /// writer that is running.
    Fresh { status: ReliefStatus },
    /// Readable and claims activity, but cannot be relied on (`cause`): a
    /// process it stopped may still be stopped. `age_ms` is the time since it
    /// was written.
    Stale {
        status: ReliefStatus,
        age_ms: u64,
        cause: ReliefStaleCause,
    },
    /// Present and not a status this build can use.
    Unreadable { reason: String },
}

/// Milliseconds since the Unix epoch; 0 if the clock is before it.
pub fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The directory both files live in, the one `host-load.v1` lives in.
pub fn default_dir() -> Option<PathBuf> {
    crate::tool_config::default_config_dir()
}

/// Writes `bytes` to `path` atomically and privately: a new file of mode 0600
/// in the same directory, synced, then renamed over the target, so a reader
/// sees the old file or the new one and never half of either, and never one
/// another user can read.
fn write_private_atomic(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a record needs a directory"))?;
    std::fs::create_dir_all(dir)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "a record needs a name"))?;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let temporary = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let result = (|| {
        let mut file = options.open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Writes `status` atomically and privately (see the module doc).
// The supervisor's writer (`relief`, built in its own lane) is the caller; this
// build reads the file and only tests write it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn write_status(dir: &Path, status: &ReliefStatus) -> io::Result<()> {
    let mut bounded = status.clone();
    let excess = bounded.failures.len().saturating_sub(MAX_FAILURES);
    bounded.failures.drain(..excess);
    let body = serde_json::to_vec(&bounded).map_err(io::Error::other)?;
    write_private_atomic(&dir.join(STATUS_FILE_NAME), &body)
}

/// What reading a private record found.
enum Record {
    Missing,
    Bytes(Vec<u8>),
    Refused(String),
}

/// Reads `path` as `host-load.v1` is read: no symlink, a regular file owned by
/// this user and private to them, at most [`MAX_BYTES`].
fn read_private(path: &Path) -> Record {
    let file = match crate::fs_entry::open_private_record_readonly(path) {
        Ok(Some(file)) => file,
        Ok(None) => return Record::Missing,
        Err(reason) => return Record::Refused(reason),
    };
    let mut bytes = Vec::new();
    match file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes) {
        Ok(_) if bytes.len() > MAX_BYTES => {
            Record::Refused(format!("larger than {MAX_BYTES} bytes"))
        }
        Ok(_) => Record::Bytes(bytes),
        Err(error) => Record::Refused(format!("could not be read: {error}")),
    }
}

/// Whether the process that wrote a status is still the one running under its
/// pid. A platform that cannot report a start time cannot say, and is taken to
/// be running: the age rule still applies there.
fn writer_running(pid: u32, start_us: u64) -> bool {
    if start_us == 0 || !cfg!(target_os = "macos") {
        return true;
    }
    super::process_start_us(pid).is_some_and(|live| live.abs_diff(start_us) <= START_SLACK_US)
}

/// [`read_status_at`] at the present moment.
pub fn read_status(dir: &Path) -> ReliefOutcome {
    read_status_at(dir, now_ms())
}

/// Reads the status as of `now_ms`.
pub fn read_status_at(dir: &Path, now_ms: u64) -> ReliefOutcome {
    read_status_with(dir, now_ms, writer_running)
}

/// [`read_status_at`] with the writer-liveness probe supplied, for tests.
pub fn read_status_with(
    dir: &Path,
    now_ms: u64,
    writer_alive: impl Fn(u32, u64) -> bool,
) -> ReliefOutcome {
    let bytes = match read_private(&dir.join(STATUS_FILE_NAME)) {
        Record::Missing => return ReliefOutcome::Absent,
        Record::Refused(reason) => {
            return ReliefOutcome::Unreadable {
                reason: format!("the status file {reason}"),
            };
        }
        Record::Bytes(bytes) => bytes,
    };
    let status: ReliefStatus = match serde_json::from_slice(&bytes) {
        Ok(status) => status,
        Err(error) => {
            return ReliefOutcome::Unreadable {
                reason: format!("the status file is not a status this build understands: {error}"),
            };
        }
    };
    if status.version != VERSION {
        return ReliefOutcome::Unreadable {
            reason: format!(
                "the status file is version {}, and this build reads version {VERSION}",
                status.version
            ),
        };
    }
    if !status.claims_activity() {
        return ReliefOutcome::Fresh { status };
    }
    let age_ms = now_ms.saturating_sub(status.written_at_ms);
    let cause = if status.written_at_ms > now_ms.saturating_add(FUTURE_SLACK_MS) {
        Some(ReliefStaleCause::ClockAhead)
    } else if age_ms > STALE_AFTER_MS {
        Some(ReliefStaleCause::Age)
    } else if !writer_alive(status.writer_pid, status.writer_start_us) {
        Some(ReliefStaleCause::WriterGone)
    } else {
        None
    };
    match cause {
        Some(cause) => ReliefOutcome::Stale {
            status,
            age_ms,
            cause,
        },
        None => ReliefOutcome::Fresh { status },
    }
}

/// The request a banner action or Health button leaves for the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRequest {
    pub reason: String,
    /// Unix epoch milliseconds, when the request was made.
    pub at_ms: u64,
}

/// Asks the supervisor to resume everything it paused. See the module
/// comment: this only leaves the request; the supervisor acts on it.
pub fn request_resume(reason: &str) -> io::Result<()> {
    let dir = default_dir().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "there is no per-user config directory (HOME unset)",
        )
    })?;
    request_resume_in(&dir, reason, now_ms())
}

/// [`request_resume`] into `dir`, as of `now_ms`.
pub fn request_resume_in(dir: &Path, reason: &str, now_ms: u64) -> io::Result<()> {
    let reason: String = reason
        .chars()
        .filter(|c| !c.is_control())
        .take(MAX_REASON)
        .collect();
    let body = serde_json::to_vec(&ResumeRequest {
        reason,
        at_ms: now_ms,
    })
    .map_err(io::Error::other)?;
    write_private_atomic(&dir.join(RESUME_REQUEST_FILE_NAME), &body)
}

/// Reads and deletes the pending resume request, the one call the supervisor
/// makes on each tick. `None` when there is none, when it is older than
/// [`REQUEST_TTL_MS`], or when it is not a request this user wrote: all three
/// delete it, so a stale or foreign request is never acted on later.
// The supervisor (built in its own lane) is the caller.
#[cfg_attr(not(test), allow(dead_code))]
pub fn take_resume_request(dir: &Path, now_ms: u64) -> Option<ResumeRequest> {
    let path = dir.join(RESUME_REQUEST_FILE_NAME);
    let record = read_private(&path);
    if matches!(record, Record::Missing) {
        return None;
    }
    let _ = std::fs::remove_file(&path);
    let Record::Bytes(bytes) = record else {
        return None;
    };
    let request: ResumeRequest = serde_json::from_slice(&bytes).ok()?;
    (now_ms.saturating_sub(request.at_ms) <= REQUEST_TTL_MS).then_some(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> FrozenTree {
        FrozenTree {
            pid: 4242,
            start_us: 1_700_000_000_000_000,
            agent_root_pid: 4000,
            terminal_session_id: Some("term-4000-ab12".into()),
            repo_path: "/work/repo".into(),
            command: "cargo test --workspace".into(),
            footprint_bytes: 9 << 30,
            since_ms: 1_000,
            advised_only: false,
        }
    }

    fn frozen_at(written: u64) -> ReliefStatus {
        ReliefStatus {
            rung: ReliefRung::Freeze,
            binding: Some(ReliefBinding::CompressorSegments),
            frozen: vec![tree()],
            slowed: 3,
            last_action: Some(ReliefAction {
                at_ms: written,
                kind: ReliefActionKind::Freeze,
                detail: "cargo test".into(),
            }),
            ..ReliefStatus::at_rest(ReliefMode::Act, written)
        }
    }

    fn alive(_: u32, _: u64) -> bool {
        true
    }

    fn read(dir: &Path, now: u64) -> ReliefOutcome {
        read_status_with(dir, now, alive)
    }

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let status = frozen_at(10_000);
        write_status(dir.path(), &status).unwrap();
        assert_eq!(read(dir.path(), 10_500), ReliefOutcome::Fresh { status });
    }

    #[test]
    fn the_wire_names_are_the_contract() {
        let json = serde_json::to_value(frozen_at(5)).unwrap();
        assert_eq!(json["mode"], "act");
        assert_eq!(json["rung"], "freeze");
        assert_eq!(json["binding"], "compressor_segments");
        assert_eq!(json["last_action"]["kind"], "freeze");
        assert_eq!(json["frozen"][0]["advised_only"], false);
        for field in ["writer_pid", "writer_start_us", "would_slow"] {
            assert!(json.get(field).is_some(), "{field} is part of the contract");
        }
        let cap = serde_json::to_value(ReliefActionKind::CapResume).unwrap();
        assert_eq!(cap, "cap_resume");
        let outcome = serde_json::to_value(ReliefOutcome::Absent).unwrap();
        assert_eq!(outcome, serde_json::json!({"state": "absent"}));
        let stale = serde_json::to_value(ReliefOutcome::Stale {
            status: frozen_at(5),
            age_ms: 9,
            cause: ReliefStaleCause::WriterGone,
        })
        .unwrap();
        assert_eq!(stale["state"], "stale");
        assert_eq!(stale["cause"], "writer_gone");
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let mut json = serde_json::to_value(frozen_at(10_000)).unwrap();
        json["added_later"] = serde_json::json!({"x": 1});
        json["frozen"][0]["also_new"] = serde_json::json!(true);
        std::fs::write(dir.path().join(STATUS_FILE_NAME), json.to_string()).unwrap();
        make_private(&dir.path().join(STATUS_FILE_NAME));
        assert!(matches!(
            read(dir.path(), 10_001),
            ReliefOutcome::Fresh { .. }
        ));
    }

    #[cfg(unix)]
    fn make_private(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_private(_: &Path) {}

    #[test]
    fn absent_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path(), 1), ReliefOutcome::Absent);
    }

    #[test]
    fn claims_activity_is_the_rung_a_real_pause_or_a_real_slowdown() {
        let rest = ReliefStatus::at_rest(ReliefMode::Act, 1);
        assert!(!rest.claims_activity());
        let mut slow_only = rest.clone();
        slow_only.slowed = 2;
        assert!(slow_only.claims_activity());
        let mut paused_under_rung_none = rest.clone();
        paused_under_rung_none.frozen = vec![tree()];
        assert!(paused_under_rung_none.claims_activity());
        let mut advised = rest.clone();
        advised.mode = ReliefMode::Advise;
        advised.would_slow = 4;
        advised.frozen = vec![FrozenTree {
            advised_only: true,
            ..tree()
        }];
        assert!(!advised.claims_activity(), "Advise changes nothing");
        let mut rung = rest;
        rung.rung = ReliefRung::Slow;
        assert!(rung.claims_activity());
    }

    #[test]
    fn a_file_claiming_activity_goes_stale_and_one_at_rest_does_not() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), &frozen_at(1_000)).unwrap();
        assert!(matches!(
            read(dir.path(), 1_000 + STALE_AFTER_MS),
            ReliefOutcome::Fresh { .. }
        ));
        match read(dir.path(), 1_000 + STALE_AFTER_MS + 1) {
            ReliefOutcome::Stale {
                status,
                age_ms,
                cause,
            } => {
                assert_eq!(age_ms, STALE_AFTER_MS + 1);
                assert_eq!(cause, ReliefStaleCause::Age);
                assert_eq!(status.paused_count(), 1);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        // The writer stops refreshing once it is at rest, so age means nothing.
        write_status(dir.path(), &ReliefStatus::at_rest(ReliefMode::Act, 1_000)).unwrap();
        assert!(matches!(
            read(dir.path(), 1_000 + 24 * 3_600_000),
            ReliefOutcome::Fresh { .. }
        ));
        // A slowdown under rung none still claims activity.
        let mut slowed = ReliefStatus::at_rest(ReliefMode::Act, 1_000);
        slowed.slowed = 1;
        write_status(dir.path(), &slowed).unwrap();
        assert!(matches!(
            read(dir.path(), 1_000 + STALE_AFTER_MS + 1),
            ReliefOutcome::Stale { .. }
        ));
    }

    #[test]
    fn a_writer_that_is_gone_makes_a_fresh_looking_claim_stale() {
        let dir = tempfile::tempdir().unwrap();
        let mut status = frozen_at(1_000);
        status.writer_pid = 77;
        status.writer_start_us = 5_000_000;
        write_status(dir.path(), &status).unwrap();
        let gone = read_status_with(dir.path(), 1_500, |pid, start| {
            assert_eq!((pid, start), (77, 5_000_000));
            false
        });
        assert!(matches!(
            gone,
            ReliefOutcome::Stale {
                cause: ReliefStaleCause::WriterGone,
                ..
            }
        ));
        // At rest, a dead writer says nothing: it wrote that it was done.
        write_status(
            dir.path(),
            &ReliefStatus {
                writer_pid: 77,
                writer_start_us: 5_000_000,
                ..ReliefStatus::at_rest(ReliefMode::Act, 1_000)
            },
        )
        .unwrap();
        assert!(matches!(
            read_status_with(dir.path(), 1_500, |_, _| false),
            ReliefOutcome::Fresh { .. }
        ));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_real_probe_knows_this_process_and_a_dead_pid() {
        let me = std::process::id();
        let start = crate::sysload::process_start_us(me).expect("this process has a start time");
        assert!(writer_running(me, start));
        assert!(!writer_running(me, start + 60_000_000));
        assert!(!writer_running(4_000_000, start));
        // No start time recorded: nothing to compare, the age rule decides.
        assert!(writer_running(4_000_000, 0));
    }

    #[test]
    fn a_status_dated_in_the_future_is_stale() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), &frozen_at(100_000)).unwrap();
        assert!(matches!(
            read(dir.path(), 100_000 - FUTURE_SLACK_MS),
            ReliefOutcome::Fresh { .. }
        ));
        assert!(matches!(
            read(dir.path(), 100_000 - FUTURE_SLACK_MS - 1),
            ReliefOutcome::Stale {
                cause: ReliefStaleCause::ClockAhead,
                ..
            }
        ));
    }

    #[test]
    fn garbage_a_wrong_version_and_a_wrong_shape_are_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATUS_FILE_NAME);
        let put = |bytes: &[u8]| {
            std::fs::write(&path, bytes).unwrap();
            make_private(&path);
        };
        put(b"{not json");
        assert!(matches!(
            read(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
        let mut json = serde_json::to_value(frozen_at(1)).unwrap();
        json["version"] = serde_json::json!(2);
        put(json.to_string().as_bytes());
        match read(dir.path(), 1) {
            ReliefOutcome::Unreadable { reason } => {
                assert!(reason.contains("version 2"), "{reason}")
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
        let mut json = serde_json::to_value(frozen_at(1)).unwrap();
        json["rung"] = serde_json::json!("melt");
        put(json.to_string().as_bytes());
        assert!(matches!(
            read(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
        let mut json = serde_json::to_value(frozen_at(1)).unwrap();
        json.as_object_mut().unwrap().remove("writer_pid");
        put(json.to_string().as_bytes());
        assert!(
            matches!(read(dir.path(), 1), ReliefOutcome::Unreadable { .. }),
            "a status that does not say who wrote it is not a status"
        );
        put(&vec![b' '; MAX_BYTES + 1]);
        assert!(matches!(
            read(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
    }

    #[test]
    fn a_directory_or_a_symlink_in_the_files_place_is_unreadable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(STATUS_FILE_NAME)).unwrap();
        assert!(matches!(
            read(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
        #[cfg(unix)]
        {
            let other = tempfile::tempdir().unwrap();
            write_status(other.path(), &frozen_at(1)).unwrap();
            let link = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(
                other.path().join(STATUS_FILE_NAME),
                link.path().join(STATUS_FILE_NAME),
            )
            .unwrap();
            assert!(matches!(
                read(link.path(), 1),
                ReliefOutcome::Unreadable { .. }
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_file_other_users_can_read_is_refused_and_the_writer_makes_private_ones() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), &frozen_at(1)).unwrap();
        let path = dir.path().join(STATUS_FILE_NAME);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "write_status is private");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        match read(dir.path(), 1) {
            ReliefOutcome::Unreadable { reason } => {
                assert!(reason.contains("accessible to other users"), "{reason}")
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
        request_resume_in(dir.path(), "x", 1).unwrap();
        let request = dir.path().join(RESUME_REQUEST_FILE_NAME);
        assert_eq!(
            std::fs::metadata(&request).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn an_advised_only_tree_is_not_paused() {
        let mut status = frozen_at(1);
        status.frozen[0].advised_only = true;
        assert_eq!(status.paused_count(), 0);
        assert_eq!(status.frozen.len(), 1);
    }

    #[test]
    fn failures_are_bounded_to_the_newest() {
        let dir = tempfile::tempdir().unwrap();
        let mut status = frozen_at(1);
        status.failures = (0..40)
            .map(|n| ReliefFailure {
                at_ms: n,
                pid: 1,
                what: format!("failure {n}"),
            })
            .collect();
        write_status(dir.path(), &status).unwrap();
        let ReliefOutcome::Fresh { status } = read(dir.path(), 1) else {
            panic!("expected Fresh");
        };
        assert_eq!(status.failures.len(), MAX_FAILURES);
        assert_eq!(status.failures.last().unwrap().at_ms, 39);
        assert_eq!(
            status.failures.first().unwrap().at_ms,
            40 - MAX_FAILURES as u64
        );
    }

    #[test]
    fn request_resume_leaves_an_atomic_request_beside_the_status() {
        let dir = tempfile::tempdir().unwrap();
        request_resume_in(dir.path(), "notification", 777).unwrap();
        let bytes = std::fs::read(dir.path().join(RESUME_REQUEST_FILE_NAME)).unwrap();
        let request: ResumeRequest = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(
            request,
            ResumeRequest {
                reason: "notification".into(),
                at_ms: 777
            }
        );
        // A second request replaces the first; no temporary file is left.
        request_resume_in(dir.path(), "health\n\u{7}button", 900).unwrap();
        let request: ResumeRequest = serde_json::from_slice(
            &std::fs::read(dir.path().join(RESUME_REQUEST_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(request.reason, "healthbutton");
        assert_eq!(request.at_ms, 900);
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
    }

    #[test]
    fn a_long_reason_is_cut() {
        let dir = tempfile::tempdir().unwrap();
        request_resume_in(dir.path(), &"x".repeat(500), 1).unwrap();
        let request: ResumeRequest = serde_json::from_slice(
            &std::fs::read(dir.path().join(RESUME_REQUEST_FILE_NAME)).unwrap(),
        )
        .unwrap();
        assert_eq!(request.reason.len(), MAX_REASON);
    }

    #[test]
    fn take_resume_request_reads_deletes_and_expires() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESUME_REQUEST_FILE_NAME);
        assert_eq!(take_resume_request(dir.path(), 1), None);

        request_resume_in(dir.path(), "health", 10_000).unwrap();
        let taken = take_resume_request(dir.path(), 10_000 + REQUEST_TTL_MS).unwrap();
        assert_eq!(taken.at_ms, 10_000);
        assert!(!path.exists(), "taken means deleted");
        assert_eq!(take_resume_request(dir.path(), 10_000), None);

        // Older than the limit: dropped, and deleted all the same.
        request_resume_in(dir.path(), "health", 10_000).unwrap();
        assert_eq!(
            take_resume_request(dir.path(), 10_000 + REQUEST_TTL_MS + 1),
            None
        );
        assert!(
            !path.exists(),
            "an expired request is not left to act later"
        );

        // Not a request: deleted, never acted on.
        std::fs::write(&path, b"{nope").unwrap();
        make_private(&path);
        assert_eq!(take_resume_request(dir.path(), 1), None);
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_request_other_users_can_write_is_deleted_unread() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(RESUME_REQUEST_FILE_NAME);
        std::fs::write(&path, br#"{"reason":"x","at_ms":5}"#).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666)).unwrap();
        assert_eq!(take_resume_request(dir.path(), 5), None);
        assert!(!path.exists());
    }
}
