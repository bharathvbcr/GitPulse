//! Each session's start and exit, on disk, until an app has recorded the exit.
//!
//! The host outlives the app; an agent can finish while no app is running. Its
//! exit status is the one fact nothing else will ever know, so the host writes
//! it down before it tells anyone: one append-only file per session,
//! `<session id>.jsonl`, `0600` in a `0700` directory. A `spawn` line when the
//! child is about to start, an `exit` line, synced, when it has been reaped.
//! The app asks for every file with an `exit` line when it connects
//! (`Request::Receipts`), records each, and then asks for it to be forgotten.
//!
//! A file with a `spawn` line and no `exit` line, for a session the host does
//! not hold, belonged to a host that died before its session did. When a host
//! starts it gives each such file the exit it never got: not reaped, with that
//! reason, which the workbench records as unresolved rather than as an exit
//! code nobody saw.

use super::proto::WireEnded;
use crate::terminal::TerminalExitPayload;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

/// Longest receipt file read: two short lines.
const MAX_RECEIPT_BYTES: u64 = 64 * 1024;
/// Most receipts one listing returns. More wait for the next connect.
pub(super) const MAX_LISTED: usize = 512;
/// What a session whose host died is recorded as having ended with.
pub(super) const HOST_ENDED: &str =
    "The terminal host ended before this session did; its exit status is unknown.";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spawned {
    session: String,
    run_id: Option<String>,
    program: String,
    cwd: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
enum Line {
    Spawn(Spawned),
    Exit(WireEnded),
}

pub(super) struct Receipts {
    dir: PathBuf,
}

impl Receipts {
    /// The receipts directory, created private and refused if it is a link.
    pub(super) fn open(dir: PathBuf) -> Result<Self, String> {
        crate::local_socket::private_dir(&dir)?;
        Ok(Self { dir })
    }

    fn path(&self, session: &str) -> Result<PathBuf, String> {
        if !crate::terminal::is_session_id(session) {
            return Err(format!("{session:?} cannot name a receipt"));
        }
        Ok(self.dir.join(format!("{session}.jsonl")))
    }

    fn append(&self, session: &str, line: &Line) -> Result<(), String> {
        let path = self.path(session)?;
        let mut options = std::fs::OpenOptions::new();
        options.append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut text = serde_json::to_string(line).map_err(|e| e.to_string())?;
        text.push('\n');
        let mut file = options.open(&path).map_err(|e| e.to_string())?;
        file.write_all(text.as_bytes())
            .and_then(|()| file.sync_all())
            .map_err(|e| format!("could not record {}: {e}", path.display()))
    }

    /// A session is about to start.
    pub(super) fn spawned(
        &self,
        session: &str,
        run_id: Option<&str>,
        program: &str,
        cwd: &str,
    ) -> Result<(), String> {
        self.append(
            session,
            &Line::Spawn(Spawned {
                session: session.to_owned(),
                run_id: run_id.map(str::to_owned),
                program: program.to_owned(),
                cwd: cwd.to_owned(),
            }),
        )
    }

    /// A session has been reaped. Synced before it returns.
    pub(super) fn exited(&self, ended: &WireEnded) -> Result<(), String> {
        self.append(&ended.payload.id, &Line::Exit(ended.clone()))
    }

    /// The app has recorded this exit.
    pub(super) fn forget(&self, session: &str) -> Result<(), String> {
        match std::fs::remove_file(self.path(session)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error.to_string()),
        }
    }

    /// A session that never started: nothing to report.
    pub(super) fn discard(&self, session: &str) {
        if let Err(error) = self.forget(session) {
            log::warn!(target: "terminal_host", "could not remove a receipt for a session that did not start: {error}");
        }
    }

    /// Every recorded exit not yet forgotten, oldest file first and at most
    /// [`MAX_LISTED`].
    pub(super) fn pending(&self) -> Vec<WireEnded> {
        let mut found: Vec<(std::time::SystemTime, WireEnded)> = self
            .files()
            .into_iter()
            .filter_map(|(path, modified)| match read(&path) {
                Ok((_, Some(ended))) => Some((modified, ended)),
                Ok((_, None)) => None,
                Err(error) => {
                    log::warn!(target: "terminal_host", "an unreadable receipt was skipped: {error}");
                    None
                }
            })
            .collect();
        found.sort_by_key(|(modified, _)| *modified);
        found.into_iter().take(MAX_LISTED).map(|(_, e)| e).collect()
    }

    /// Gives every session this host does not hold, and whose file has no
    /// exit, the exit its dead host never wrote. Run once, at start, before
    /// the first session is spawned. Returns how many were closed.
    pub(super) fn recover(&self, live: &HashSet<String>) -> usize {
        let mut closed = 0;
        for (path, _) in self.files() {
            let Ok((Some(spawned), None)) = read(&path) else {
                continue;
            };
            if live.contains(&spawned.session) {
                continue;
            }
            let ended = WireEnded {
                payload: TerminalExitPayload {
                    id: spawned.session.clone(),
                    exit_code: None,
                    signal: String::new(),
                    error: Some(HOST_ENDED.into()),
                    reaped: false,
                    ended_by_gitpulse: false,
                },
                ended_by_gitpulse: false,
                program: spawned.program,
                cwd: spawned.cwd,
                run_id: spawned.run_id,
            };
            match self.exited(&ended) {
                Ok(()) => closed += 1,
                Err(error) => {
                    log::warn!(target: "terminal_host", "could not close a receipt its host left open: {error}")
                }
            }
        }
        closed
    }

    /// Receipt files and their modification times, bounded.
    fn files(&self) -> Vec<(PathBuf, std::time::SystemTime)> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_suffix(".jsonl"))
                    .is_some_and(crate::terminal::is_session_id)
            })
            .take(MAX_LISTED * 4)
            .filter_map(|entry| {
                let meta = entry.metadata().ok()?;
                meta.is_file().then(|| {
                    (
                        entry.path(),
                        meta.modified().unwrap_or(std::time::UNIX_EPOCH),
                    )
                })
            })
            .collect()
    }
}

/// A receipt's spawn line and exit line, either of which may be missing.
fn read(path: &Path) -> Result<(Option<Spawned>, Option<WireEnded>), String> {
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    if !meta.file_type().is_file() || meta.len() > MAX_RECEIPT_BYTES {
        return Err(format!(
            "{} is not a receipt this host wrote",
            path.display()
        ));
    }
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let mut spawned = None;
    let mut ended = None;
    for line in std::io::BufReader::new(file).lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<Line>(&line) {
            Ok(Line::Spawn(s)) => spawned = Some(s),
            // The first exit is the one that happened; a later one could
            // only be `recover` racing a reaper that got there first.
            Ok(Line::Exit(e)) => {
                ended.get_or_insert(e);
            }
            // A torn last line from a crash mid-write is not a receipt line.
            Err(_) => {}
        }
    }
    Ok((spawned, ended))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A temporary directory with no symlink in its path, as the host's own
    /// directory has: `private_dir` refuses one (macOS's `/var` is a link).
    fn private_tempdir() -> tempfile::TempDir {
        tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
    }

    fn ended(id: &str, code: Option<i32>, run: Option<&str>) -> WireEnded {
        WireEnded {
            payload: TerminalExitPayload {
                id: id.into(),
                exit_code: code,
                signal: String::new(),
                error: None,
                reaped: true,
                ended_by_gitpulse: false,
            },
            ended_by_gitpulse: false,
            program: "claude".into(),
            cwd: "/w".into(),
            run_id: run.map(str::to_owned),
        }
    }

    #[test]
    fn an_exit_is_kept_until_forgotten_and_a_live_session_is_never_listed() {
        let dir = private_tempdir();
        let receipts = Receipts::open(dir.path().join("receipts")).unwrap();
        receipts
            .spawned("term-1-a", Some("run-1"), "claude", "/w")
            .unwrap();
        receipts.spawned("term-1-b", None, "zsh", "/w").unwrap();
        assert!(receipts.pending().is_empty(), "nothing has exited");
        receipts
            .exited(&ended("term-1-a", Some(3), Some("run-1")))
            .unwrap();
        let pending = receipts.pending();
        assert_eq!(pending, vec![ended("term-1-a", Some(3), Some("run-1"))]);
        receipts.forget("term-1-a").unwrap();
        receipts.forget("term-1-a").unwrap();
        assert!(receipts.pending().is_empty());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&dir.path().join("receipts")), 0o700);
            assert_eq!(mode(&dir.path().join("receipts/term-1-b.jsonl")), 0o600);
        }
    }

    #[test]
    fn a_session_its_host_died_under_gets_an_unreaped_exit_and_a_live_one_does_not() {
        let dir = private_tempdir();
        let receipts = Receipts::open(dir.path().to_path_buf()).unwrap();
        receipts
            .spawned("term-1-a", Some("run-1"), "claude", "/w")
            .unwrap();
        receipts.spawned("term-1-b", None, "zsh", "/w").unwrap();
        receipts.spawned("term-1-c", None, "zsh", "/w").unwrap();
        receipts.exited(&ended("term-1-c", Some(0), None)).unwrap();
        let live = HashSet::from(["term-1-b".to_string()]);
        assert_eq!(receipts.recover(&live), 1);
        assert_eq!(receipts.recover(&live), 0, "recovery is idempotent");
        let pending = receipts.pending();
        let dead = pending.iter().find(|e| e.payload.id == "term-1-a").unwrap();
        assert!(!dead.payload.reaped);
        assert_eq!(dead.payload.exit_code, None);
        assert_eq!(dead.payload.error.as_deref(), Some(HOST_ENDED));
        assert_eq!(dead.run_id.as_deref(), Some("run-1"));
        assert!(pending.iter().all(|e| e.payload.id != "term-1-b"));
        let clean = pending.iter().find(|e| e.payload.id == "term-1-c").unwrap();
        assert_eq!(
            clean.payload.exit_code,
            Some(0),
            "a real exit is not overwritten"
        );
    }

    #[test]
    fn names_that_are_not_session_ids_and_torn_lines_are_refused_or_skipped() {
        let dir = private_tempdir();
        let receipts = Receipts::open(dir.path().to_path_buf()).unwrap();
        for bad in ["../x", "term-1-a/../../y", "", "term-x-1", "term-1-A"] {
            assert!(receipts.spawned(bad, None, "zsh", "/w").is_err(), "{bad:?}");
        }
        receipts.exited(&ended("term-2-1", Some(1), None)).unwrap();
        let path = dir.path().join("term-2-1.jsonl");
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .unwrap();
        file.write_all(b"{\"exit\":{\"payl").unwrap();
        assert_eq!(receipts.pending(), vec![ended("term-2-1", Some(1), None)]);
    }
}
