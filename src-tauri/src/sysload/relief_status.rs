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
//! # Honesty
//!
//! A reader never turns "could not tell" into "nothing is paused".
//! [`ReliefOutcome`] has four states: [`Absent`](ReliefOutcome::Absent) (no
//! supervisor has ever written one), [`Fresh`](ReliefOutcome::Fresh),
//! [`Stale`](ReliefOutcome::Stale) and [`Unreadable`](ReliefOutcome::Unreadable).
//! The writer refreshes the file every tick while its rung is not `none` and
//! writes once more on the transition to `none`, so a file that claims a rung
//! and is older than [`STALE_AFTER_MS`] is Stale: its writer may be gone with a
//! process still stopped. A file whose rung is `none` is at rest, and its age
//! says nothing.
//!
//! # The resume seam
//!
//! The "Resume now" action of the banner and the Health button cannot call the
//! supervisor from another process, and the supervisor's `resume_all` is not
//! in this module's reach. [`request_resume`] writes `host-relief-resume.request`
//! (`{reason, at_ms}`) atomically beside the status file. The supervisor
//! consumes it on its next tick: it calls `relief::resume_all(true, reason)`
//! and removes the file. Until the backend consumes it the request does
//! nothing, and nothing in this module reads it back. Any process of the same
//! user can write the file, so the supervisor decides how far to trust one
//! (it can only end a pause, never start one).

use serde::{Deserialize, Serialize};
use std::io;
use std::path::{Path, PathBuf};

/// The file's name, beside `host-load.v1`.
pub const STATUS_FILE_NAME: &str = "host-relief.v1.json";
/// The request a banner or Health button leaves for the supervisor.
pub const RESUME_REQUEST_FILE_NAME: &str = "host-relief-resume.request";
/// The version this build writes and the only one it reads.
pub const VERSION: u32 = 1;
/// A file that claims activity and is older than this is Stale.
pub const STALE_AFTER_MS: u64 = 30_000;
/// The most failures the file carries, newest last.
// Used by `write_status`, whose caller is the supervisor's writer.
#[cfg_attr(not(test), allow(dead_code))]
pub const MAX_FAILURES: usize = 16;
/// The most bytes read from the file; a larger one is Unreadable.
const MAX_BYTES: usize = 256 * 1024;
/// The longest reason a resume request carries.
const MAX_REASON: usize = 64;

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
    pub repo_path: String,
    pub command: String,
    pub footprint_bytes: u64,
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
    pub at_ms: u64,
    pub kind: ReliefActionKind,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliefFailure {
    pub at_ms: u64,
    pub pid: u32,
    pub what: String,
}

/// The published status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReliefStatus {
    pub version: u32,
    pub written_at_ms: u64,
    pub mode: ReliefMode,
    pub rung: ReliefRung,
    pub binding: Option<ReliefBinding>,
    pub slowed: u32,
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
            mode,
            rung: ReliefRung::None,
            binding: None,
            slowed: 0,
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
    /// what makes its age matter.
    pub fn claims_activity(&self) -> bool {
        self.rung != ReliefRung::None
    }
}

/// What a read found. Serialised for IPC as `{state, ...}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReliefOutcome {
    /// No file: no supervisor has written one here.
    Absent,
    /// Readable, and either at rest or refreshed within [`STALE_AFTER_MS`].
    Fresh { status: ReliefStatus },
    /// Readable, claims activity, and was last written `age_ms` ago: the
    /// writer may be gone, so a process it stopped may still be stopped.
    Stale { status: ReliefStatus, age_ms: u64 },
    /// Present and not a status this build can use.
    Unreadable { reason: String },
}

impl ReliefOutcome {
    /// Whether the answer to "is anything paused" is known.
    pub fn is_checked(&self) -> bool {
        matches!(self, Self::Absent | Self::Fresh { .. })
    }
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

/// Writes `status` atomically (`devmap_query::write_atomic`: a unique
/// temporary name, then a rename), so a reader sees the old file or the new
/// one and never half of either.
// The supervisor's writer (`relief`, built in its own lane) is the caller; this
// build reads the file and only tests write it.
#[cfg_attr(not(test), allow(dead_code))]
pub fn write_status(dir: &Path, status: &ReliefStatus) -> io::Result<()> {
    let mut bounded = status.clone();
    let excess = bounded.failures.len().saturating_sub(MAX_FAILURES);
    bounded.failures.drain(..excess);
    let body = serde_json::to_vec(&bounded).map_err(io::Error::other)?;
    devmap_query::write_atomic(&dir.join(STATUS_FILE_NAME), &body).map(|_| ())
}

/// [`read_status_at`] at the present moment.
pub fn read_status(dir: &Path) -> ReliefOutcome {
    read_status_at(dir, now_ms())
}

/// Reads the status as of `now_ms`.
pub fn read_status_at(dir: &Path, now_ms: u64) -> ReliefOutcome {
    use crate::fs_entry::{read_bounded_nofollow, BoundedRead};
    let path = dir.join(STATUS_FILE_NAME);
    let bytes = match read_bounded_nofollow(&path, MAX_BYTES) {
        Ok(bytes) => bytes,
        Err(BoundedRead::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            return ReliefOutcome::Absent;
        }
        Err(BoundedRead::Io(error)) => {
            return ReliefOutcome::Unreadable {
                reason: format!("the status file could not be read: {error}"),
            };
        }
        Err(BoundedRead::NotRegular) => {
            return ReliefOutcome::Unreadable {
                reason: "the status file is not a regular file".into(),
            };
        }
        Err(BoundedRead::TooLarge | BoundedRead::Grew) => {
            return ReliefOutcome::Unreadable {
                reason: format!("the status file is larger than {MAX_BYTES} bytes"),
            };
        }
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
    let age_ms = now_ms.saturating_sub(status.written_at_ms);
    if status.claims_activity() && age_ms > STALE_AFTER_MS {
        return ReliefOutcome::Stale { status, age_ms };
    }
    ReliefOutcome::Fresh { status }
}

/// The request a banner action or Health button leaves for the supervisor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResumeRequest {
    pub reason: String,
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
    devmap_query::write_atomic(&dir.join(RESUME_REQUEST_FILE_NAME), &body).map(|_| ())
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

    #[test]
    fn round_trips_through_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let status = frozen_at(10_000);
        write_status(dir.path(), &status).unwrap();
        assert_eq!(
            read_status_at(dir.path(), 10_500),
            ReliefOutcome::Fresh { status }
        );
    }

    #[test]
    fn the_wire_names_are_the_contract() {
        let json = serde_json::to_value(frozen_at(5)).unwrap();
        assert_eq!(json["mode"], "act");
        assert_eq!(json["rung"], "freeze");
        assert_eq!(json["binding"], "compressor_segments");
        assert_eq!(json["last_action"]["kind"], "freeze");
        assert_eq!(json["frozen"][0]["advised_only"], false);
        let cap = serde_json::to_value(ReliefActionKind::CapResume).unwrap();
        assert_eq!(cap, "cap_resume");
        let outcome = serde_json::to_value(ReliefOutcome::Absent).unwrap();
        assert_eq!(outcome, serde_json::json!({"state": "absent"}));
    }

    #[test]
    fn unknown_fields_are_tolerated() {
        let dir = tempfile::tempdir().unwrap();
        let mut json = serde_json::to_value(frozen_at(10_000)).unwrap();
        json["added_later"] = serde_json::json!({"x": 1});
        json["frozen"][0]["also_new"] = serde_json::json!(true);
        std::fs::write(dir.path().join(STATUS_FILE_NAME), json.to_string()).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 10_001),
            ReliefOutcome::Fresh { .. }
        ));
    }

    #[test]
    fn absent_when_no_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_status_at(dir.path(), 1), ReliefOutcome::Absent);
    }

    #[test]
    fn a_file_claiming_activity_goes_stale_and_one_at_rest_does_not() {
        let dir = tempfile::tempdir().unwrap();
        write_status(dir.path(), &frozen_at(1_000)).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1_000 + STALE_AFTER_MS),
            ReliefOutcome::Fresh { .. }
        ));
        match read_status_at(dir.path(), 1_000 + STALE_AFTER_MS + 1) {
            ReliefOutcome::Stale { status, age_ms } => {
                assert_eq!(age_ms, STALE_AFTER_MS + 1);
                assert_eq!(status.paused_count(), 1);
            }
            other => panic!("expected Stale, got {other:?}"),
        }
        // The writer stops refreshing once it is at rest, so age means nothing.
        write_status(dir.path(), &ReliefStatus::at_rest(ReliefMode::Act, 1_000)).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1_000 + 24 * 3_600_000),
            ReliefOutcome::Fresh { .. }
        ));
        // A clock that moved backwards is not an age.
        assert!(matches!(
            read_status_at(dir.path(), 0),
            ReliefOutcome::Fresh { .. }
        ));
    }

    #[test]
    fn garbage_a_wrong_version_and_a_wrong_shape_are_unreadable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(STATUS_FILE_NAME);
        std::fs::write(&path, b"{not json").unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
        let mut json = serde_json::to_value(frozen_at(1)).unwrap();
        json["version"] = serde_json::json!(2);
        std::fs::write(&path, json.to_string()).unwrap();
        match read_status_at(dir.path(), 1) {
            ReliefOutcome::Unreadable { reason } => {
                assert!(reason.contains("version 2"), "{reason}")
            }
            other => panic!("expected Unreadable, got {other:?}"),
        }
        let mut json = serde_json::to_value(frozen_at(1)).unwrap();
        json["rung"] = serde_json::json!("melt");
        std::fs::write(&path, json.to_string()).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
        std::fs::write(&path, vec![b' '; MAX_BYTES + 1]).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1),
            ReliefOutcome::Unreadable { .. }
        ));
    }

    #[test]
    fn a_directory_or_a_symlink_in_the_files_place_is_unreadable_not_absent() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(STATUS_FILE_NAME)).unwrap();
        assert!(matches!(
            read_status_at(dir.path(), 1),
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
                read_status_at(link.path(), 1),
                ReliefOutcome::Unreadable { .. }
            ));
        }
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
        let ReliefOutcome::Fresh { status } = read_status_at(dir.path(), 1) else {
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
}
