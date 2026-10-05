//! Live updates for task writes made by other processes.
//!
//! `workbench-changed` fired only for writes this process made. A task an
//! agent filed over MCP — a separate process writing the same profile — showed
//! up only after the window lost and regained focus, and an agent working in
//! one of GitPulse's own terminal tabs never takes focus away, so for the
//! commonest case the task did not appear until the person clicked elsewhere.
//!
//! SQLite's `data_version` changes, on a connection, whenever *another*
//! connection commits — in this process or any other. This thread holds a
//! connection of its own that never writes, polls that counter while the
//! window is on screen, and announces a change. It reads no table, so it does
//! not depend on Manvi's schema, and it never creates the profile.

use super::WorkbenchState;
use std::path::Path;
use std::time::Duration;
use tauri::{Emitter, Manager};

const VISIBLE_PERIOD: Duration = Duration::from_millis(1_000);
const HIDDEN_PERIOD: Duration = Duration::from_millis(2_000);

/// Start the watcher. A thread that fails to start costs live updates only:
/// focus and reload still refresh, so this logs rather than failing setup.
pub(crate) fn install(app: &tauri::AppHandle) {
    let host = app.state::<WorkbenchState>().inner().clone();
    let app = app.clone();
    if let Err(error) = std::thread::Builder::new()
        .name("workbench-external-changes".into())
        .spawn(move || run(host, app))
    {
        log::warn!(target: "workbench", "external change watcher did not start: {error}");
    }
}

fn run(host: WorkbenchState, app: tauri::AppHandle) {
    let mut watcher = Watcher::default();
    let mut last_error: Option<String> = None;
    while host.check_open().is_ok() {
        let visible = app.get_webview_window("main").is_some_and(|window| {
            window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
        });
        if !visible {
            std::thread::sleep(HIDDEN_PERIOD);
            continue;
        }
        let path = match host.0.path.clone() {
            Some(path) => Ok(path),
            None => super::intake::default_profile_path().map_err(|e| e.message),
        };
        match path.and_then(|path| watcher.poll(&path)) {
            Ok(true) => {
                last_error = None;
                if let Err(error) = app.emit(
                    "workbench-changed",
                    serde_json::json!({"sequence": null, "source": "external"}),
                ) {
                    log::warn!(target: "workbench", "external change notification failed: {error}");
                }
            }
            Ok(false) => last_error = None,
            Err(error) => {
                if last_error.as_deref() != Some(error.as_str()) {
                    log::warn!(target: "workbench", "external change watcher: {error}");
                }
                last_error = Some(error);
            }
        }
        std::thread::sleep(VISIBLE_PERIOD);
    }
}

/// One read-only observer of the profile's commit counter.
#[derive(Default)]
pub(crate) struct Watcher {
    connection: Option<(rusqlite::Connection, Option<(u64, u64)>)>,
    last: Option<i64>,
}

impl Watcher {
    /// True when another connection committed since the previous poll.
    ///
    /// The first successful poll only takes a baseline. A missing profile is
    /// "nothing to report", and is never created. A profile replaced on disk
    /// is reopened, and that replacement itself counts as a change.
    pub(crate) fn poll(&mut self, path: &Path) -> Result<bool, String> {
        let identity = match std::fs::metadata(path) {
            Ok(meta) => file_identity(&meta),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let had = self.connection.take().is_some();
                return Ok(had);
            }
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        let replaced = self
            .connection
            .as_ref()
            .is_some_and(|(_, held)| held.is_some() && *held != identity);
        if replaced {
            self.connection = None;
        }
        if self.connection.is_none() {
            let connection = open(path)?;
            self.connection = Some((connection, identity));
            self.last = None;
        }
        let (connection, _) = self.connection.as_ref().expect("connection just ensured");
        let version: i64 = match connection.query_row("PRAGMA data_version", [], |row| row.get(0)) {
            Ok(version) => version,
            Err(error) => {
                self.connection = None;
                return Err(format!("cannot read the profile's change counter: {error}"));
            }
        };
        let changed = replaced || self.last.is_some_and(|last| last != version);
        self.last = Some(version);
        Ok(changed)
    }
}

fn open(path: &Path) -> Result<rusqlite::Connection, String> {
    use rusqlite::OpenFlags;
    // No SQLITE_OPEN_CREATE: launching the app must not create a profile.
    let connection = rusqlite::Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| format!("cannot open {}: {e}", path.display()))?;
    connection
        .busy_timeout(Duration::from_millis(250))
        .map_err(|e| format!("cannot configure the change watcher: {e}"))?;
    Ok(connection)
}

#[cfg(unix)]
fn file_identity(meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn file_identity(_meta: &std::fs::Metadata) -> Option<(u64, u64)> {
    // Windows cannot delete a database another connection holds open.
    None
}

#[cfg(test)]
mod tests {
    use super::Watcher;

    fn write(path: &std::path::Path, value: i64) {
        let connection = rusqlite::Connection::open(path).unwrap();
        connection
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE IF NOT EXISTS t(v INTEGER);")
            .unwrap();
        connection
            .execute("INSERT INTO t(v) VALUES (?1)", [value])
            .unwrap();
    }

    #[test]
    fn a_commit_from_another_connection_is_seen_once() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        let mut watcher = Watcher::default();
        assert!(
            !watcher.poll(&path).unwrap(),
            "the first poll is a baseline"
        );
        assert!(!watcher.poll(&path).unwrap());
        write(&path, 2);
        assert!(watcher.poll(&path).unwrap());
        assert!(
            !watcher.poll(&path).unwrap(),
            "one commit, one announcement"
        );
        for value in 0..50 {
            write(&path, value);
        }
        assert!(watcher.poll(&path).unwrap(), "a burst coalesces into one");
        assert!(!watcher.poll(&path).unwrap());
    }

    #[test]
    fn a_missing_profile_is_quiet_and_never_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        let mut watcher = Watcher::default();
        for _ in 0..3 {
            assert!(!watcher.poll(&path).unwrap());
        }
        assert!(!path.exists());
        write(&path, 1);
        assert!(
            !watcher.poll(&path).unwrap(),
            "first sight of a new profile is a baseline"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_replaced_profile_is_reopened_and_announced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        let mut watcher = Watcher::default();
        assert!(!watcher.poll(&path).unwrap());
        let staged = dir.path().join("staged.sqlite");
        write(&staged, 9);
        std::fs::rename(&staged, &path).unwrap();
        assert!(watcher.poll(&path).unwrap());
        write(&path, 10);
        assert!(
            watcher.poll(&path).unwrap(),
            "the reopened connection sees later commits"
        );
        std::fs::remove_file(&path).unwrap();
        assert!(
            watcher.poll(&path).unwrap(),
            "a profile that vanished is a change too"
        );
    }
}
