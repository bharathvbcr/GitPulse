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
//!
//! What a pass costs matters more than how often it runs, because the pass
//! runs for as long as the window is up. A pass is two `stat` calls (the
//! database and its write-ahead log, which is where a WAL-mode commit lands)
//! and, only when either changed, the sqlite read; and it does not ask the
//! main thread whether the window is visible, which is a round trip through
//! the event loop. Window events say it ([`note_main_window_on_screen`]), and
//! a real query corrects them every [`VERIFY_EVERY`].

use super::WorkbenchState;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};
use tauri::{Emitter, Manager};

const VISIBLE_PERIOD: Duration = Duration::from_millis(1_000);
const HIDDEN_PERIOD: Duration = Duration::from_millis(2_000);
/// How often the real window state replaces what events have said. Events
/// cover every way this app shows and hides the window; the operating system
/// minimising it is not one of them, and that only costs cheap passes.
const VERIFY_EVERY: Duration = Duration::from_secs(30);
/// A file written this recently when it was last looked at is read regardless
/// of whether its size and modification time moved: a second write inside the
/// filesystem's timestamp granularity would leave both as they were. The same
/// "racy" rule git applies to its index.
const RACY: Duration = Duration::from_secs(2);

/// Whether the main window is on screen, as events have last said. Starts true
/// because the window is created visible; `--background` launches say
/// otherwise as soon as they hide it.
static MAIN_ON_SCREEN: AtomicBool = AtomicBool::new(true);

/// Records that the main window was shown, focused or hidden.
pub(crate) fn note_main_window_on_screen(on_screen: bool) {
    MAIN_ON_SCREEN.store(on_screen, Ordering::Relaxed);
}

/// The window's state for the polling loop: the last event's word, replaced by
/// a real query once [`VERIFY_EVERY`] has passed since the last one.
struct Presence {
    verified: Option<Instant>,
}

impl Presence {
    fn on_screen(&mut self, now: Instant, query: impl FnOnce() -> bool) -> bool {
        if self
            .verified
            .is_none_or(|at| now.saturating_duration_since(at) >= VERIFY_EVERY)
        {
            self.verified = Some(now);
            MAIN_ON_SCREEN.store(query(), Ordering::Relaxed);
        }
        MAIN_ON_SCREEN.load(Ordering::Relaxed)
    }
}

/// Start the watcher. A thread that fails to start costs live updates only:
/// focus and reload still refresh, so this logs rather than failing setup.
pub(crate) fn install(app: &tauri::AppHandle) {
    let host = app.state::<WorkbenchState>().inner().clone();
    let app = app.clone();
    if let Err(error) = crate::supervisor::spawn_service("workbench-external-changes", move || {
        run(host.clone(), app.clone())
    }) {
        log::warn!(target: "workbench", "external change watcher did not start: {error}");
    }
}

fn run(host: WorkbenchState, app: tauri::AppHandle) {
    let mut watcher = Watcher::default();
    let mut retention = super::retention::Retention::new();
    let mut last_error: Option<String> = None;
    let mut presence = Presence { verified: None };
    while host.check_open().is_ok() {
        // Daily retention, hidden window or not; the pass runs on its own thread.
        retention.tick(&host);
        let visible = presence.on_screen(Instant::now(), || {
            app.get_webview_window("main").is_some_and(|window| {
                window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
            })
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
    /// What the files looked like when `last` was read, and when that was.
    seen: Option<(Fingerprint, SystemTime)>,
    /// `PRAGMA data_version` reads, for a test to count.
    reads: u64,
}

/// Size and modification time of one file.
type Stamp = (u64, Option<SystemTime>);

/// The database and its write-ahead log, as `stat` sees them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Fingerprint {
    database: Stamp,
    wal: Option<Stamp>,
}

impl Fingerprint {
    fn of(database: &std::fs::Metadata, path: &Path) -> Self {
        let stamp = |meta: &std::fs::Metadata| (meta.len(), meta.modified().ok());
        Self {
            database: stamp(database),
            wal: std::fs::metadata(wal_of(path)).ok().as_ref().map(stamp),
        }
    }

    /// True when a file was written close enough to `looked_at` that an
    /// unchanged fingerprint proves nothing (see [`RACY`]). A file with no
    /// modification time, or one from the future, is always racy.
    fn racy(&self, looked_at: SystemTime) -> bool {
        [Some(self.database), self.wal]
            .into_iter()
            .flatten()
            .any(|(_, modified)| match modified {
                None => true,
                Some(at) => looked_at.duration_since(at).map_or(true, |age| age < RACY),
            })
    }
}

impl Watcher {
    /// True when another connection committed since the previous poll.
    ///
    /// The first successful poll only takes a baseline. A missing profile is
    /// "nothing to report", and is never created. A profile replaced on disk
    /// is reopened, and that replacement itself counts as a change.
    ///
    /// A profile whose files look exactly as they did when the counter was
    /// last read, and were not written in the moments before, is not asked
    /// again: nothing can have committed without touching them.
    pub(crate) fn poll(&mut self, path: &Path) -> Result<bool, String> {
        let meta = match std::fs::metadata(path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.seen = None;
                let had = self.connection.take().is_some();
                return Ok(had);
            }
            Err(error) => return Err(format!("cannot inspect {}: {error}", path.display())),
        };
        let identity = file_identity(&meta);
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
            self.seen = None;
        }
        let fingerprint = Fingerprint::of(&meta, path);
        if let Some((before, looked_at)) = self.seen {
            if before == fingerprint && !fingerprint.racy(looked_at) {
                return Ok(false);
            }
        }
        let (connection, _) = self.connection.as_ref().expect("connection just ensured");
        self.reads += 1;
        let version: i64 = match connection.query_row("PRAGMA data_version", [], |row| row.get(0)) {
            Ok(version) => version,
            Err(error) => {
                self.connection = None;
                self.seen = None;
                return Err(format!("cannot read the profile's change counter: {error}"));
            }
        };
        let changed = replaced || self.last.is_some_and(|last| last != version);
        self.last = Some(version);
        self.seen = Some((fingerprint, SystemTime::now()));
        Ok(changed)
    }
}

/// The write-ahead log beside `path`, which is where a WAL-mode commit lands.
fn wal_of(path: &Path) -> std::path::PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push("-wal");
    name.into()
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

    /// The loop used to ask the main thread about the window on every pass.
    /// Events answer instead, and a real query only corrects them now and then.
    #[test]
    fn window_events_answer_and_a_real_query_only_corrects_them_now_and_then() {
        use super::{note_main_window_on_screen, Presence, VERIFY_EVERY};
        use std::time::Instant;
        let start = Instant::now();
        let mut presence = Presence { verified: None };
        let mut asked = 0;
        // First pass: nothing has been verified, so the real state is taken.
        assert!(!presence.on_screen(start, || {
            asked += 1;
            false
        }));
        // Events move the answer without a query.
        note_main_window_on_screen(true);
        assert!(presence.on_screen(start + VERIFY_EVERY / 2, || {
            asked += 1;
            false
        }));
        note_main_window_on_screen(false);
        assert!(!presence.on_screen(start + VERIFY_EVERY / 2, || {
            asked += 1;
            true
        }));
        assert_eq!(asked, 1, "events should have answered without a query");
        // Later, the real state replaces whatever the events last said.
        assert!(presence.on_screen(start + VERIFY_EVERY, || {
            asked += 1;
            true
        }));
        assert_eq!(asked, 2);
        note_main_window_on_screen(true);
    }

    /// Backdates `path` so it no longer looks recently written.
    fn settle(path: &std::path::Path) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(3600);
        for candidate in [path.to_path_buf(), super::wal_of(path)] {
            if let Ok(file) = std::fs::OpenOptions::new().write(true).open(&candidate) {
                file.set_modified(old).unwrap();
            }
        }
    }

    /// The poll costs a sqlite statement per second for as long as the window
    /// is up. A profile nobody has written to has no reason to be asked.
    #[test]
    fn an_unchanged_profile_is_not_asked_for_its_change_counter() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        let mut watcher = Watcher::default();
        assert!(!watcher.poll(&path).unwrap(), "the baseline");
        // Opening the profile is the observer's own first touch of it (it
        // brings the write-ahead log into being); let that age out, as it does
        // within seconds in the running app, and take one pass over the result.
        settle(&path);
        assert!(!watcher.poll(&path).unwrap());
        let before = watcher.reads;
        for _ in 0..6 {
            assert!(!watcher.poll(&path).unwrap());
        }
        assert_eq!(
            watcher.reads - before,
            0,
            "an unchanged profile was read {} times in 6 polls",
            watcher.reads - before
        );
    }

    /// The gate is only sound if reading the counter does not itself move the
    /// two files it watches.
    #[test]
    fn reading_the_change_counter_leaves_the_fingerprint_alone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        let mut watcher = Watcher::default();
        watcher.poll(&path).unwrap();
        settle(&path);
        watcher.poll(&path).unwrap();
        let settled = watcher.seen.map(|(fingerprint, _)| fingerprint);
        assert!(settled.is_some());
        for _ in 0..3 {
            watcher.seen = None;
            watcher.poll(&path).unwrap();
            assert_eq!(watcher.seen.map(|(fingerprint, _)| fingerprint), settled);
        }
    }

    /// Two commits inside one timestamp tick leave the same size and the same
    /// mtime, so a file this fresh cannot be trusted to have stayed the same.
    #[test]
    fn a_profile_written_a_moment_ago_is_read_again_even_if_its_files_look_the_same() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        let mut watcher = Watcher::default();
        for _ in 0..3 {
            assert!(!watcher.poll(&path).unwrap());
        }
        assert_eq!(watcher.reads, 3);
    }

    #[test]
    fn a_commit_to_a_settled_profile_is_still_seen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("workbench.sqlite");
        write(&path, 1);
        settle(&path);
        let mut watcher = Watcher::default();
        for _ in 0..3 {
            assert!(!watcher.poll(&path).unwrap());
        }
        write(&path, 2);
        assert!(watcher.poll(&path).unwrap(), "the commit was gated away");
        settle(&path);
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
