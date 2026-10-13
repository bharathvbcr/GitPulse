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
    if let Err(error) = std::fs::create_dir_all(dir) {
        return Acquire::Unavailable(format!("cannot create {}: {error}", dir.display()));
    }
    let path = dir.join(LOCK_FILE);
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
    let path = dir.join(LOCK_FILE);
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
        // We took it, so nobody held it; dropping `file` gives it back.
        Ok(true) => Ok(None),
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

    /// Waits, with a bound, for the lock in `dir` to be free.
    ///
    /// `flock` belongs to the open file description, not the descriptor, so a
    /// child another test forked while the lock was held (a PTY child between
    /// fork and exec) keeps it until its exec closes the close-on-exec
    /// descriptor. Under load that window is long, so the release is awaited
    /// instead of asserted at once.
    fn released(dir: &Path) -> bool {
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        loop {
            if !status_in(dir, "1.4.0").running {
                return true;
            }
            if std::time::Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// The window `released` exists for, made deterministic: a child forked
    /// while the lock is held keeps it after its owner dropped it, until the
    /// child execs.
    #[cfg(unix)]
    #[test]
    fn a_child_forked_under_the_lock_holds_it_until_it_execs() {
        use std::os::unix::process::CommandExt;
        let dir = tempfile::tempdir().unwrap();
        let first = match acquire(dir.path(), "1.4.0") {
            Acquire::Held(lock) => lock,
            other => panic!("the first daemon was not admitted: {other:?}"),
        };
        let mut fds = [0i32; 2];
        // SAFETY: `fds` is a valid two-element buffer for pipe(2).
        assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0);
        let (read_fd, write_fd) = (fds[0], fds[1]);
        let spawner = std::thread::spawn(move || {
            let mut command = std::process::Command::new("true");
            // SAFETY: the closure runs between fork and exec and calls only
            // the async-signal-safe write and usleep.
            unsafe {
                command.pre_exec(move || {
                    libc::write(write_fd, b"x".as_ptr().cast(), 1);
                    libc::usleep(300_000);
                    Ok(())
                });
            }
            command.status()
        });
        let mut byte = [0u8; 1];
        // SAFETY: `byte` is a valid one-byte buffer and `read_fd` is open.
        let read = unsafe { libc::read(read_fd, byte.as_mut_ptr().cast(), 1) };
        assert_eq!(read, 1, "the child never reached its pre-exec hook");
        drop(first);
        assert!(
            status_in(dir.path(), "1.4.0").running,
            "a forked child holds the open file description, so the lock outlives its owner"
        );
        spawner.join().unwrap().unwrap();
        assert!(released(dir.path()));
        // SAFETY: both descriptors were opened above and are closed once.
        unsafe {
            libc::close(read_fd);
            libc::close(write_fd);
        }
    }

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
        assert!(released(dir.path()));
        assert!(matches!(acquire(dir.path(), "1.4.1"), Acquire::Held(_)));
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
