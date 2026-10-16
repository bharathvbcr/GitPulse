//! The one-owner lock for `gitpulsed`.
//!
//! Nothing started, stopped or tracked the daemon: two copies ran silently
//! against one ledger, and a daemon left over from before an upgrade kept
//! writing with the old build's idea of the transcript cursors. A running
//! daemon now holds an exclusive lock on `gitpulsed.lock` in its profile
//! directory for its whole life; the kernel drops it when the process ends,
//! so there is no stale-lock cleanup to get wrong. The file also names the
//! owner (`pid`, then the build's version) so the app can say which daemon is
//! running and whether it is the app's own version.

use serde::Serialize;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const LOCK_FILE: &str = "gitpulsed.lock";

/// The longest the owner record is read: pid and version are a few bytes.
const MAX_RECORD: u64 = 256;

/// A probe holds the lock for an instant; a daemon starting at that instant
/// asks again before concluding someone else owns it.
const RETRIES: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_millis(40);

/// Whoever holds the lock.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Owner {
    pub pid: Option<u32>,
    pub version: Option<String>,
}

/// The outcome of [`acquire`].
#[derive(Debug)]
pub enum Acquire {
    /// This process is the daemon. Dropping the value releases the lock.
    Held(DaemonLock),
    /// Another daemon holds it.
    Occupied(Owner),
    /// The lock could not be checked at all, which is not "nobody holds it".
    Unavailable(String),
}

#[derive(Debug)]
pub struct DaemonLock {
    _file: File,
    path: PathBuf,
}

impl DaemonLock {
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for DaemonLock {
    fn drop(&mut self) {
        unlock(&self._file);
    }
}

fn open(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Share nothing: a second open fails while the first is held, which
        // is the whole lock.
        options.share_mode(0);
    }
    options.open(path)
}

/// `Ok(true)` locked, `Ok(false)` held elsewhere.
#[cfg(unix)]
fn try_lock(file: &File) -> std::io::Result<bool> {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: a valid descriptor owned by `file` for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.kind() {
            std::io::ErrorKind::WouldBlock => return Ok(false),
            std::io::ErrorKind::Interrupted => continue,
            _ => return Err(error),
        }
    }
}

/// On Windows the exclusive open is the lock; holding the handle is holding it.
#[cfg(windows)]
fn try_lock(_file: &File) -> std::io::Result<bool> {
    Ok(true)
}

/// Gives the lock back now, whoever else still holds a descriptor to the file.
///
/// An `flock` belongs to the open file description, not to one descriptor, and
/// closing a descriptor releases it only when it is the last. The file is
/// opened close-on-exec, but a process forked on another thread between the
/// open and the close (the terminal's `fork`, a `posix_spawn`) copies the
/// descriptor and keeps the copy until its `exec` runs. Releasing by close
/// alone left the lock held for that long: a probe saw a daemon that had
/// already stopped, and a daemon starting at that instant could be refused as
/// a second owner. `LOCK_UN` ends the lock on the description itself.
#[cfg(unix)]
fn unlock(file: &File) {
    use std::os::unix::io::AsRawFd;
    loop {
        // SAFETY: a valid descriptor owned by `file` for the whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_UN) };
        // Nothing to report on failure: closing the descriptor, which follows,
        // is the release this falls back to.
        if rc == 0 || std::io::Error::last_os_error().kind() != std::io::ErrorKind::Interrupted {
            return;
        }
    }
}

/// On Windows the handle is the lock, and handles are not inherited by copy.
#[cfg(windows)]
fn unlock(_file: &File) {}

fn read_owner(path: &Path) -> Owner {
    let mut text = String::new();
    let read = File::open(path).and_then(|file| file.take(MAX_RECORD).read_to_string(&mut text));
    let mut lines = text.lines();
    let (pid, version) = if read.is_ok() {
        (
            lines.next().and_then(|l| l.trim().parse().ok()),
            lines
                .next()
                .map(|l| {
                    l.trim()
                        .chars()
                        .filter(|c| !c.is_control())
                        .collect::<String>()
                })
                .filter(|v| !v.is_empty()),
        )
    } else {
        (None, None)
    };
    Owner { pid, version }
}

/// Takes the daemon lock in `dir` for a process running `version`.
pub fn acquire(dir: &Path, version: &str) -> Acquire {
    acquire_named(dir, LOCK_FILE, version)
}

/// [`acquire`] for another one-owner process, under its own file name in
/// `dir`: the terminal host takes one per protocol version.
pub fn acquire_named(dir: &Path, file: &str, version: &str) -> Acquire {
    if let Err(error) = std::fs::create_dir_all(dir) {
        return Acquire::Unavailable(format!("cannot create {}: {error}", dir.display()));
    }
    let path = dir.join(file);
    for attempt in 0..RETRIES {
        let file = match open(&path) {
            Ok(file) => file,
            #[cfg(windows)]
            Err(_) if path.exists() => {
                if attempt + 1 < RETRIES {
                    std::thread::sleep(RETRY_PAUSE);
                    continue;
                }
                return Acquire::Occupied(read_owner(&path));
            }
            Err(error) => {
                return Acquire::Unavailable(format!("cannot open {}: {error}", path.display()))
            }
        };
        match try_lock(&file) {
            Ok(true) => {
                let mut file = file;
                let record = format!("{}\n{version}\n", std::process::id());
                let written = file
                    .set_len(0)
                    .and_then(|()| file.write_all(record.as_bytes()))
                    .and_then(|()| file.flush());
                if let Err(error) = written {
                    unlock(&file);
                    return Acquire::Unavailable(format!(
                        "cannot record the owner in {}: {error}",
                        path.display()
                    ));
                }
                return Acquire::Held(DaemonLock { _file: file, path });
            }
            Ok(false) if attempt + 1 < RETRIES => std::thread::sleep(RETRY_PAUSE),
            Ok(false) => return Acquire::Occupied(read_owner(&path)),
            Err(error) => {
                return Acquire::Unavailable(format!("cannot lock {}: {error}", path.display()))
            }
        }
    }
    Acquire::Occupied(read_owner(&path))
}

/// The daemon running against `dir` right now, or `None` when nobody holds
/// the lock. `Err` when the lock could not be examined.
pub fn running(dir: &Path) -> Result<Option<Owner>, String> {
    running_named(dir, LOCK_FILE)
}

/// [`running`] for the owner of the lock `file` in `dir`.
pub fn running_named(dir: &Path, file: &str) -> Result<Option<Owner>, String> {
    let path = dir.join(file);
    if !path.exists() {
        return Ok(None);
    }
    let file = match open(&path) {
        Ok(file) => file,
        #[cfg(windows)]
        Err(_) => return Ok(Some(read_owner(&path))),
        #[cfg(not(windows))]
        Err(error) => return Err(format!("cannot open {}: {error}", path.display())),
    };
    match try_lock(&file) {
        // We took it, so nobody held it. The probe gives it back by unlocking
        // rather than by closing alone, or a process forked meanwhile would
        // hold it for the daemon that starts next.
        Ok(true) => {
            unlock(&file);
            Ok(None)
        }
        Ok(false) => Ok(Some(read_owner(&path))),
        Err(error) => Err(format!("cannot lock {}: {error}", path.display())),
    }
}

/// What the app shows about the daemon.
#[derive(Debug, Clone, Serialize)]
pub struct DaemonStatus {
    pub running: bool,
    pub pid: Option<u32>,
    pub version: Option<String>,
    pub app_version: &'static str,
    /// A daemon is running and is not this build: it writes with another
    /// build's rules and should be restarted.
    pub mismatch: bool,
    /// Set when the lock could not be examined: not the same as "not running".
    pub error: Option<String>,
}

pub fn status_in(dir: &Path, app_version: &'static str) -> DaemonStatus {
    match running(dir) {
        Ok(None) => DaemonStatus {
            running: false,
            pid: None,
            version: None,
            app_version,
            mismatch: false,
            error: None,
        },
        Ok(Some(owner)) => DaemonStatus {
            running: true,
            mismatch: owner.version.as_deref() != Some(app_version),
            pid: owner.pid,
            version: owner.version,
            app_version,
            error: None,
        },
        Err(error) => DaemonStatus {
            running: false,
            pid: None,
            version: None,
            app_version,
            mismatch: false,
            error: Some(error),
        },
    }
}

/// Where the daemon's lock lives: its own directory in the app's config dir.
pub fn default_dir() -> Option<PathBuf> {
    crate::tool_config::default_config_dir().map(|dir| dir.join("gitpulsed"))
}

/// The Tauri command the diagnostics view reads.
#[tauri::command]
pub fn cmd_daemon_status() -> DaemonStatus {
    let app_version = env!("CARGO_PKG_VERSION");
    match default_dir() {
        Some(dir) => status_in(&dir, app_version),
        None => DaemonStatus {
            running: false,
            pid: None,
            version: None,
            app_version,
            mismatch: false,
            error: Some("Application config directory unavailable".into()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The defect: nothing stopped a second daemon. A second acquire is
    /// refused, names the first owner, and succeeds once the first is gone.
    #[test]
    fn a_second_daemon_is_refused_and_names_the_first() {
        let dir = tempfile::tempdir().unwrap();
        let first = match acquire(dir.path(), "1.4.0") {
            Acquire::Held(lock) => lock,
            other => panic!("the first daemon was not admitted: {other:?}"),
        };
        match acquire(dir.path(), "1.4.1") {
            Acquire::Occupied(owner) => {
                assert_eq!(owner.pid, Some(std::process::id()));
                assert_eq!(owner.version.as_deref(), Some("1.4.0"));
            }
            other => panic!("a second daemon ran beside the first: {other:?}"),
        }
        let status = status_in(dir.path(), "1.4.1");
        assert!(status.running && status.mismatch, "{status:?}");
        assert!(!status_in(dir.path(), "1.4.0").mismatch);
        drop(first);
        assert!(!status_in(dir.path(), "1.4.0").running);
        assert!(matches!(acquire(dir.path(), "1.4.1"), Acquire::Held(_)));
    }

    /// The flake from the full suite: a process forked on another thread
    /// while the lock was held copies the descriptor and keeps the copy until
    /// its `exec`; closing ours then left the lock held, and the daemon read
    /// as still running. A duplicated descriptor shares the open file
    /// description exactly as a forked copy does, so it stands in for the
    /// child deterministically.
    #[cfg(unix)]
    #[test]
    fn releasing_the_lock_does_not_wait_for_a_forked_copy_of_the_descriptor() {
        let dir = tempfile::tempdir().unwrap();
        let first = match acquire(dir.path(), "1.4.0") {
            Acquire::Held(lock) => lock,
            other => panic!("the first daemon was not admitted: {other:?}"),
        };
        let forked_copy = first._file.try_clone().unwrap();
        assert!(status_in(dir.path(), "1.4.0").running);
        drop(first);
        assert!(
            !status_in(dir.path(), "1.4.0").running,
            "a copy of the descriptor kept a stopped daemon's lock held"
        );
        assert!(matches!(acquire(dir.path(), "1.4.1"), Acquire::Held(_)));
        drop(forked_copy);
    }

    #[test]
    fn an_unwritable_directory_is_unavailable_not_free() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("not-a-directory");
        std::fs::write(&file, "x").unwrap();
        assert!(matches!(
            acquire(&file.join("sub"), "1"),
            Acquire::Unavailable(_)
        ));
    }

    #[test]
    fn an_owner_record_is_bounded_and_printable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(LOCK_FILE);
        std::fs::write(&path, format!("77\n1.\u{1b}[31m{}\n", "x".repeat(5000))).unwrap();
        let owner = read_owner(&path);
        assert_eq!(owner.pid, Some(77));
        let version = owner.version.unwrap();
        assert!(version.len() <= MAX_RECORD as usize && !version.chars().any(char::is_control));
        // Garbage names no one rather than the wrong one.
        std::fs::write(&path, "not a pid\n\n").unwrap();
        assert_eq!(
            read_owner(&path),
            Owner {
                pid: None,
                version: None
            }
        );
    }
}
