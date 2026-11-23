//! Who ended a terminal session, and a durable record of every session's life.
//!
//! Two pieces live here because they answer one question ("who closed my
//! sessions?") that the app log (about eight hours at 1 MB x 2) and the
//! 64-row session ledger could not:
//!
//! - [`KillOrigin`] names the route a close came by. Every kill carries one,
//!   so a log line, a lifecycle record and the ledger row all say the same
//!   thing, and a protected session can tell a direct click from a batch.
//! - [`RotatingJsonl`] is the bounded append-only file behind the lifecycle
//!   log and the ledger's eviction archive. A write never fails its caller:
//!   an I/O error is logged and the kill or spawn goes on.
//!
//! Nothing here changes the terminal host's wire protocol: attribution is
//! recorded by the app, which is the only side that knows the origin.

use serde::{Deserialize, Serialize};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

/// Lifecycle log file, beside the session ledger.
pub const LIFECYCLE_FILE: &str = "terminal-lifecycle.v1.jsonl";
/// Rows the ledger evicted past its cap, beside the session ledger.
pub const ARCHIVE_FILE: &str = "terminal-sessions.v1.archive.jsonl";
/// Largest size of the live file before it rotates.
pub const ROTATE_BYTES: u64 = 8 * 1024 * 1024;
/// Files kept, the live one included: at most `ROTATE_BYTES * ROTATE_FILES` on disk.
pub const ROTATE_FILES: usize = 4;

/// The route a session close came by.
///
/// `Unspecified` is what a caller that does not say sends (the existing
/// frontend callers of `cmd_terminal_kill`); it is treated as a direct close.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KillOrigin {
    /// The user closed one tab.
    TabClose,
    /// "Close others" / "Close all" and the like: one click, many sessions.
    BatchClose,
    /// Stop on a row of the Sessions list.
    SessionsListStop,
    /// "Stop children" on a session's spawned children.
    StopChildren,
    /// An agent closed a child through the MCP `gitpulse_terminal_close` tool
    /// or `gitpulse-hook term close`. Both arrive as one orchestrate call, so
    /// they are not told apart.
    AgentClose,
    /// An agent's `exec` cleaned up the child it spawned for that command.
    AgentExecCleanup,
    /// The host load governor (parking an idle session).
    LoadGovernor,
    /// The app quit with "stop everything".
    QuitStopAll,
    /// The caller did not say.
    #[default]
    Unspecified,
}

impl KillOrigin {
    /// Stable lowercase name for logs and records.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TabClose => "tab_close",
            Self::BatchClose => "batch_close",
            Self::SessionsListStop => "sessions_list_stop",
            Self::StopChildren => "stop_children",
            Self::AgentClose => "agent_close",
            Self::AgentExecCleanup => "agent_exec_cleanup",
            Self::LoadGovernor => "load_governor",
            Self::QuitStopAll => "quit_stop_all",
            Self::Unspecified => "unspecified",
        }
    }

    /// Whether a protected session survives this close. The routes where one
    /// action or one automation reaches many sessions respect it; a person
    /// closing a single tab, and quit, do not. `AgentExecCleanup` closes a
    /// child the same call just opened, which no one could have protected.
    pub fn respects_protection(self) -> bool {
        matches!(
            self,
            Self::BatchClose | Self::StopChildren | Self::AgentClose | Self::LoadGovernor
        )
    }
}

impl std::fmt::Display for KillOrigin {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What a close did: it ran, or it was refused because the session is
/// protected from this route. A refusal is not an error: the caller reports
/// the session as skipped and says why. `cmd_terminal_kill` returns it to the
/// frontend as `"killed"` or `"skipped_protected"`, so nothing reads an error's
/// text to learn a session was spared.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum KillOutcome {
    Killed,
    SkippedProtected,
}

/// Message a caller shows for a session a protected flag spared.
pub fn skipped_protected_message(session_id: &str, origin: KillOrigin) -> String {
    format!(
        "session {session_id} is protected and was not closed by {origin}; close its tab directly to stop it"
    )
}

/// A bounded, append-only JSONL file: the live file rotates to `.1`, `.2` and
/// so on at `max_bytes`, and the oldest is dropped past `files`.
pub struct RotatingJsonl {
    path: PathBuf,
    max_bytes: u64,
    files: usize,
    /// Serializes appends and rotations from this process.
    write: Mutex<()>,
}

impl RotatingJsonl {
    pub fn new(path: PathBuf, max_bytes: u64, files: usize) -> Self {
        Self {
            path,
            max_bytes: max_bytes.max(1),
            files: files.max(1),
            write: Mutex::new(()),
        }
    }

    /// The production bound: 8 MB x 4.
    pub fn standard(path: PathBuf) -> Self {
        Self::new(path, ROTATE_BYTES, ROTATE_FILES)
    }

    /// Appends one line. Never fails the caller: an error is logged.
    pub fn append(&self, line: &str) {
        if let Err(error) = self.try_append(line) {
            log::warn!(
                target: "terminal",
                "could not append to {}: {error}",
                self.path.display()
            );
        }
    }

    fn rotated(&self, n: usize) -> PathBuf {
        let mut name = self.path.clone().into_os_string();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    }

    fn try_append(&self, line: &str) -> std::io::Result<()> {
        let _held = self
            .write
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let current = match fs::symlink_metadata(&self.path) {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_file() => {
                return Err(std::io::Error::other(
                    "refusing a symlinked or non-file log",
                ));
            }
            Ok(meta) => meta.len(),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => 0,
            Err(error) => return Err(error),
        };
        // Size is read from disk each time: the file may have been rotated or
        // removed behind our back.
        let needed = line.len() as u64 + 1;
        if current > 0 && current + needed > self.max_bytes {
            self.rotate()?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&self.path)?;
        let mut buf = String::with_capacity(line.len() + 1);
        buf.push_str(line);
        buf.push('\n');
        file.write_all(buf.as_bytes())
    }

    fn rotate(&self) -> std::io::Result<()> {
        if self.files <= 1 {
            return fs::remove_file(&self.path);
        }
        let oldest = self.rotated(self.files - 1);
        match fs::remove_file(&oldest) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        for n in (1..self.files - 1).rev() {
            match fs::rename(self.rotated(n), self.rotated(n + 1)) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        fs::rename(&self.path, self.rotated(1))
    }

    /// Every retained line, oldest first. For tests and tooling.
    pub fn read_all(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for n in (1..self.files).rev() {
            if let Ok(text) = fs::read_to_string(self.rotated(n)) {
                lines.extend(text.lines().map(str::to_owned));
            }
        }
        if let Ok(text) = fs::read_to_string(&self.path) {
            lines.extend(text.lines().map(str::to_owned));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotation_keeps_the_newest_files_and_a_bounded_total() {
        let dir = tempfile::tempdir().unwrap();
        let log = RotatingJsonl::new(dir.path().join("l.jsonl"), 64, 3);
        for i in 0..40 {
            log.append(&format!("{{\"n\":{i:04}}}"));
        }
        let lines = log.read_all();
        assert!(lines.last().unwrap().contains("0039"), "{lines:?}");
        assert!(!lines.first().unwrap().contains("0000"), "oldest dropped");
        let files = fs::read_dir(dir.path()).unwrap().count();
        assert!(files <= 3, "at most three files, got {files}");
        for n in 0..3 {
            let path = if n == 0 {
                dir.path().join("l.jsonl")
            } else {
                dir.path().join(format!("l.jsonl.{n}"))
            };
            if let Ok(meta) = fs::metadata(&path) {
                assert!(meta.len() <= 64, "{} over the bound", path.display());
            }
        }
    }

    #[test]
    fn an_unwritable_path_is_logged_not_raised() {
        let dir = tempfile::tempdir().unwrap();
        // A directory where the file should be: every append fails.
        fs::create_dir(dir.path().join("l.jsonl")).unwrap();
        RotatingJsonl::new(dir.path().join("l.jsonl"), 64, 2).append("{}");
    }

    #[test]
    fn origins_round_trip_and_only_batch_routes_respect_protection() {
        for origin in [
            KillOrigin::TabClose,
            KillOrigin::BatchClose,
            KillOrigin::SessionsListStop,
            KillOrigin::StopChildren,
            KillOrigin::AgentClose,
            KillOrigin::AgentExecCleanup,
            KillOrigin::LoadGovernor,
            KillOrigin::QuitStopAll,
            KillOrigin::Unspecified,
        ] {
            let text = serde_json::to_string(&origin).unwrap();
            assert_eq!(text, format!("\"{}\"", origin.as_str()));
            assert_eq!(serde_json::from_str::<KillOrigin>(&text).unwrap(), origin);
        }
        assert!(!KillOrigin::TabClose.respects_protection());
        assert!(!KillOrigin::Unspecified.respects_protection());
        assert!(!KillOrigin::QuitStopAll.respects_protection());
        assert!(KillOrigin::BatchClose.respects_protection());
        assert!(KillOrigin::LoadGovernor.respects_protection());
    }
}
