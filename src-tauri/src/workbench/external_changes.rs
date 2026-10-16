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
//!
//! The same goes for how often the thread wakes at all. On screen it passes
//! once a second, which is how soon an external task appears. Off screen (the
//! common idle state: hidden, or in the background) there is nothing to
//! announce, so it blocks on a [`Beacon`] that a show event or the host closing
//! signals, bounded by the next real window check and the next retention look,
//! and wakes about twice a minute instead of every two seconds.

#[doc(hidden)]
pub mod bench_seam;

use super::WorkbenchState;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};
use tauri::{Emitter, Manager};

const VISIBLE_PERIOD: Duration = Duration::from_millis(1_000);
/// The shortest hidden wait, so a clock that reads wrongly cannot spin the loop.
const MIN_WAIT: Duration = Duration::from_millis(1);
/// How often the real window state replaces what events have said. Events
/// cover every way this app shows and hides the window; the operating system
/// minimising it is not one of them, and that only costs cheap passes.
const VERIFY_EVERY: Duration = Duration::from_secs(30);
/// A file written this recently when it was last looked at is read regardless
/// of whether its size and modification time moved: a second write inside the
/// filesystem's timestamp granularity would leave both as they were. The same
/// "racy" rule git applies to its index.
const RACY: Duration = Duration::from_secs(2);

/// What the loop waits on: the window's state as events have said it, and a
/// counter that moves on every such event (and on shutdown).
///
/// The counter is what makes a wait safe. The loop reads it *before* it looks
/// at the window and waits for it to differ from what it read, so an event that
/// lands anywhere after that read either shows in the look or ends the wait at
/// once; there is no gap for it to fall through. The wait runs to a deadline
/// around a condition variable, so a spurious wakeup is only a re-check.
pub(crate) struct Beacon {
    on_screen: AtomicBool,
    /// Moves on every event. Held while `on_screen` is written, so the two
    /// are never seen out of step by [`Beacon::correct`].
    generation: Mutex<u64>,
    moved: Condvar,
}

impl Beacon {
    /// `on_screen` is the state before any event has said otherwise.
    pub(crate) const fn new(on_screen: bool) -> Self {
        Self {
            on_screen: AtomicBool::new(on_screen),
            generation: Mutex::new(0),
            moved: Condvar::new(),
        }
    }

    /// An event said the window is, or is not, on screen. Every event counts,
    /// including one that repeats the current state, because a real query that
    /// is in flight may have read the state before it.
    pub(crate) fn set_on_screen(&self, on_screen: bool) {
        let mut generation = crate::supervisor::lock_or_recover(&self.generation);
        self.on_screen.store(on_screen, Ordering::Release);
        *generation = generation.wrapping_add(1);
        self.moved.notify_all();
    }

    /// Ends the wait without a window event: the host is shutting down.
    pub(crate) fn wake(&self) {
        let mut generation = crate::supervisor::lock_or_recover(&self.generation);
        *generation = generation.wrapping_add(1);
        self.moved.notify_all();
    }

    fn on_screen(&self) -> bool {
        self.on_screen.load(Ordering::Acquire)
    }

    fn generation(&self) -> u64 {
        *crate::supervisor::lock_or_recover(&self.generation)
    }

    /// Replaces the state with a real query's answer, unless an event arrived
    /// after `seen`: that event is newer than the answer.
    fn correct(&self, seen: u64, real: bool) {
        let generation = crate::supervisor::lock_or_recover(&self.generation);
        if *generation == seen {
            self.on_screen.store(real, Ordering::Release);
        }
    }

    /// Blocks until the counter differs from `seen` or `timeout` passes, and
    /// says whether it moved.
    fn wait(&self, seen: u64, timeout: Duration) -> bool {
        let deadline = Instant::now().checked_add(timeout);
        let mut generation = crate::supervisor::lock_or_recover(&self.generation);
        loop {
            if *generation != seen {
                return true;
            }
            let left = deadline.map_or(timeout, |at| at.saturating_duration_since(Instant::now()));
            if left.is_zero() {
                return false;
            }
            generation = self
                .moved
                .wait_timeout(generation, left)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        }
    }
}

/// The window's state as events have last said it. Starts true because the
/// window is created visible; `--background` launches say otherwise as soon as
/// they hide it.
static BEACON: Beacon = Beacon::new(true);

/// Records that the main window was shown, focused or hidden, and wakes the
/// loop if it is waiting out a hidden window.
pub(crate) fn note_main_window_on_screen(on_screen: bool) {
    BEACON.set_on_screen(on_screen);
}

/// Wakes the loop so it sees that the host closed.
pub(crate) fn wake() {
    BEACON.wake();
}

/// The window's state for the polling loop: the last event's word, replaced by
/// a real query once [`VERIFY_EVERY`] has passed since the last one.
struct Presence {
    verified: Option<Instant>,
}

impl Presence {
    /// `seen` is the beacon's counter as the loop read it before this call.
    fn on_screen(
        &mut self,
        beacon: &Beacon,
        seen: u64,
        now: Instant,
        query: impl FnOnce() -> bool,
    ) -> bool {
        if self.next_verify_in(now).is_zero() {
            self.verified = Some(now);
            beacon.correct(seen, query());
        }
        beacon.on_screen()
    }

    /// How long until the real window is next asked about.
    fn next_verify_in(&self, now: Instant) -> Duration {
        self.verified.map_or(Duration::ZERO, |at| {
            VERIFY_EVERY.saturating_sub(now.saturating_duration_since(at))
        })
    }
}

/// Start the watcher. A thread that fails to start costs live updates only:
/// focus and reload still refresh, so this logs rather than failing setup.
pub(crate) fn install(app: &tauri::AppHandle) {
    let host = app.state::<WorkbenchState>().inner().clone();
    let app = app.clone();
    if let Err(error) = crate::supervisor::spawn_service("workbench-external-changes", move || {
        drive(
            &mut Live {
                host: host.clone(),
                app: app.clone(),
                watcher: Watcher::default(),
                retention: super::retention::Retention::new(),
            },
            &BEACON,
        )
    }) {
        log::warn!(target: "workbench", "external change watcher did not start: {error}");
    }
}

/// What the loop needs from the world. The loop is the schedule; this is every
/// way it touches anything, so a test or a bench can run the schedule without
/// a window system.
pub(crate) trait Env {
    /// False once the task host is shutting down.
    fn is_open(&self) -> bool;
    fn now(&self) -> Instant;
    /// Daily retention's own clock, hidden window or not.
    fn retention_tick(&mut self, now: Instant);
    /// How long until retention next wants a tick.
    fn retention_next_in(&self, now: Instant) -> Duration;
    /// The real window state, which events only approximate.
    fn query_window(&self) -> bool;
    /// One pass of the commit-counter watcher.
    fn poll(&mut self) -> Result<bool, String>;
    /// Tells the frontend another process wrote the profile.
    fn announce(&self) -> Result<(), String>;
    /// Waits for `beacon` to move past `seen`, for at most `timeout`. A test
    /// replaces this to run on a clock of its own.
    fn wait(&mut self, beacon: &Beacon, seen: u64, timeout: Duration) {
        beacon.wait(seen, timeout);
    }
}

/// The loop: retention, then a presence check, then either a pass or a wait.
///
/// While the window is on screen the wait is [`VISIBLE_PERIOD`]: an external
/// task has to show within about a second, and a pass is two `stat` calls. While
/// it is not, nothing here can have anything to announce, so the wait runs to
/// the next thing that is due on its own (the real window check or the
/// retention clock) and is cut short by a window event or by shutdown.
pub(crate) fn drive(env: &mut impl Env, beacon: &Beacon) {
    let mut last_error: Option<String> = None;
    let mut presence = Presence { verified: None };
    loop {
        // Before the host is asked whether it is open and before the window is
        // looked at, so nothing can slip between: a close that lands after this
        // read ends the wait at once, and one that landed before it is seen by
        // `is_open`.
        let seen = beacon.generation();
        if !env.is_open() {
            break;
        }
        let now = env.now();
        env.retention_tick(now);
        if !presence.on_screen(beacon, seen, now, || env.query_window()) {
            let due = presence
                .next_verify_in(now)
                .min(env.retention_next_in(now))
                .max(MIN_WAIT);
            env.wait(beacon, seen, due);
            continue;
        }
        match env.poll() {
            Ok(true) => {
                last_error = None;
                if let Err(error) = env.announce() {
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
        env.wait(beacon, seen, VISIBLE_PERIOD);
    }
}

/// The running app's side of [`Env`].
struct Live {
    host: WorkbenchState,
    app: tauri::AppHandle,
    watcher: Watcher,
    retention: super::retention::Retention,
}

impl Env for Live {
    fn is_open(&self) -> bool {
        self.host.check_open().is_ok()
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn retention_tick(&mut self, _now: Instant) {
        self.retention.tick(&self.host);
    }
    fn retention_next_in(&self, now: Instant) -> Duration {
        self.retention.next_check_in(now)
    }
    fn query_window(&self) -> bool {
        self.app.get_webview_window("main").is_some_and(|window| {
            window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(true)
        })
    }
    fn poll(&mut self) -> Result<bool, String> {
        let path = match self.host.0.path.clone() {
            Some(path) => Ok(path),
            None => super::intake::default_profile_path().map_err(|e| e.message),
        };
        path.and_then(|path| self.watcher.poll(&path))
    }
    fn announce(&self) -> Result<(), String> {
        self.app
            .emit(
                "workbench-changed",
                serde_json::json!({"sequence": null, "source": "external"}),
            )
            .map_err(|e| e.to_string())
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
        use super::Presence;
        let start = Instant::now();
        let beacon = Beacon::new(true);
        let mut presence = Presence { verified: None };
        let mut asked = 0;
        // First pass: nothing has been verified, so the real state is taken.
        assert!(
            !presence.on_screen(&beacon, beacon.generation(), start, || {
                asked += 1;
                false
            })
        );
        // Events move the answer without a query.
        beacon.set_on_screen(true);
        assert!(presence.on_screen(
            &beacon,
            beacon.generation(),
            start + VERIFY_EVERY / 2,
            || {
                asked += 1;
                false
            }
        ));
        beacon.set_on_screen(false);
        assert!(!presence.on_screen(
            &beacon,
            beacon.generation(),
            start + VERIFY_EVERY / 2,
            || {
                asked += 1;
                true
            }
        ));
        assert_eq!(asked, 1, "events should have answered without a query");
        // Later, the real state replaces whatever the events last said.
        assert!(
            presence.on_screen(&beacon, beacon.generation(), start + VERIFY_EVERY, || {
                asked += 1;
                true
            })
        );
        assert_eq!(asked, 2);
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

    /// The loop on a clock of its own: `wait` advances a virtual time instead of
    /// sleeping, and scripted events land at set offsets from the start.
    struct Sim<'a> {
        beacon: &'a Beacon,
        start: Instant,
        now: Instant,
        end: Instant,
        /// Window events still to come: offset from the start, and what they say.
        events: Vec<(Duration, bool)>,
        /// The real window, which an event also changes.
        window: Cell<bool>,
        /// An event that lands while the loop is asking the real window.
        during_query: Cell<Option<bool>>,
        /// An event that lands after the presence check, before the wait.
        before_wait: Cell<Option<bool>>,
        retention: Retention,
        retention_checks: u32,
        waits: Vec<Duration>,
        polls: u32,
        first_poll_at: Option<Duration>,
        /// A real watcher over a profile, for the tests that need a commit to
        /// be seen; `poll` answers `false` without one.
        watch: Option<(std::path::PathBuf, Watcher)>,
        /// Commits another process makes: offset from the start, and the value.
        writes: Vec<(Duration, i64)>,
        /// When each announcement was made, as an offset from the start.
        announced: std::cell::RefCell<Vec<Duration>>,
    }

    impl<'a> Sim<'a> {
        fn new(beacon: &'a Beacon, window: bool, run_for: Duration) -> Self {
            let start = Instant::now();
            Self {
                beacon,
                start,
                now: start,
                end: start + run_for,
                events: Vec::new(),
                window: Cell::new(window),
                during_query: Cell::new(None),
                before_wait: Cell::new(None),
                retention: Retention::new(),
                retention_checks: 0,
                waits: Vec::new(),
                polls: 0,
                first_poll_at: None,
                watch: None,
                writes: Vec::new(),
                announced: std::cell::RefCell::new(Vec::new()),
            }
        }
    }

    impl Env for Sim<'_> {
        fn is_open(&self) -> bool {
            self.now < self.end
        }
        fn now(&self) -> Instant {
            self.now
        }
        fn retention_tick(&mut self, now: Instant) {
            if self.retention.due_to_check(now) {
                self.retention_checks += 1;
            }
        }
        fn retention_next_in(&self, now: Instant) -> Duration {
            if let Some(on) = self.before_wait.take() {
                self.window.set(on);
                self.beacon.set_on_screen(on);
            }
            self.retention.next_check_in(now)
        }
        fn query_window(&self) -> bool {
            let answer = self.window.get();
            if let Some(on) = self.during_query.take() {
                self.window.set(on);
                self.beacon.set_on_screen(on);
            }
            answer
        }
        fn poll(&mut self) -> Result<bool, String> {
            self.polls += 1;
            self.first_poll_at
                .get_or_insert(self.now.duration_since(self.start));
            match self.watch.as_mut() {
                Some((path, watcher)) => watcher.poll(path),
                None => Ok(false),
            }
        }
        fn announce(&self) -> Result<(), String> {
            self.announced
                .borrow_mut()
                .push(self.now.duration_since(self.start));
            Ok(())
        }
        fn wait(&mut self, beacon: &Beacon, seen: u64, timeout: Duration) {
            self.waits.push(timeout);
            // Commits land as virtual time passes, wherever the loop was.
            let reach = self.now + timeout;
            while let Some(&(offset, value)) = self.writes.first() {
                if self.start + offset > reach {
                    break;
                }
                self.writes.remove(0);
                if let Some((path, _)) = &self.watch {
                    write(path, value);
                }
            }
            if beacon.generation() != seen {
                return;
            }
            match self.events.first().copied() {
                Some((offset, on)) if self.start + offset <= self.now + timeout => {
                    self.events.remove(0);
                    self.now = self.now.max(self.start + offset);
                    self.window.set(on);
                    beacon.set_on_screen(on);
                }
                _ => self.now += timeout,
            }
        }
    }

    use super::{Beacon, Env, VERIFY_EVERY, VISIBLE_PERIOD};
    use crate::workbench::retention::Retention;
    use std::cell::Cell;
    use std::time::{Duration, Instant};

    /// The defect: a hidden window was polled every two seconds for nothing.
    #[test]
    fn a_hidden_window_for_a_minute_costs_at_most_three_rounds_and_no_passes() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(60));
        super::drive(&mut sim, &beacon);
        assert!(
            sim.waits.len() <= 3,
            "{} rounds in a simulated minute: {:?}",
            sim.waits.len(),
            sim.waits
        );
        assert_eq!(sim.polls, 0, "a hidden window has nothing to announce");
        assert!(sim.waits.iter().all(|wait| *wait <= VERIFY_EVERY));
    }

    #[test]
    fn a_visible_window_is_still_passed_over_once_a_second() {
        let beacon = Beacon::new(true);
        let mut sim = Sim::new(&beacon, true, Duration::from_secs(60));
        super::drive(&mut sim, &beacon);
        assert_eq!(sim.polls, 60);
        assert!(sim.waits.iter().all(|wait| *wait == VISIBLE_PERIOD));
    }

    #[test]
    fn a_show_event_resumes_the_passes_at_that_moment() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(20));
        sim.events.push((Duration::from_secs(10), true));
        super::drive(&mut sim, &beacon);
        assert_eq!(sim.first_poll_at, Some(Duration::from_secs(10)));
        assert_eq!(sim.polls, 10, "one a second from the show to the end");
    }

    /// An event after the loop read the counter but before it looked at the
    /// window: the look's own answer was already stale, and the event must win.
    #[test]
    fn a_show_event_during_the_real_query_is_not_overwritten_by_it() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(5));
        sim.during_query.set(Some(true));
        super::drive(&mut sim, &beacon);
        assert_eq!(sim.first_poll_at, Some(Duration::ZERO));
    }

    /// An event after the look, before the wait: the wait must not start.
    #[test]
    fn a_show_event_between_the_look_and_the_wait_is_not_lost() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(5));
        sim.before_wait.set(Some(true));
        super::drive(&mut sim, &beacon);
        assert_eq!(
            sim.first_poll_at,
            Some(Duration::ZERO),
            "the loop slept through a show event"
        );
    }

    #[test]
    fn retention_keeps_its_own_clock_while_the_window_is_hidden() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(25 * 60));
        super::drive(&mut sim, &beacon);
        assert_eq!(
            sim.retention_checks, 3,
            "a check at the start and one each ten minutes"
        );
        assert_eq!(sim.polls, 0);
    }

    #[test]
    fn a_wait_ends_when_the_counter_moves_and_not_before() {
        let beacon = Beacon::new(false);
        let seen = beacon.generation();
        let started = Instant::now();
        assert!(!beacon.wait(seen, Duration::from_millis(20)));
        assert!(started.elapsed() >= Duration::from_millis(20));
        beacon.wake();
        assert!(beacon.wait(seen, Duration::from_secs(60)), "already moved");
    }

    fn until(within: Duration, mut done: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + within;
        while !done() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        done()
    }

    /// The hidden loop sits in a wait of up to half a minute; showing the
    /// window must not wait for it.
    #[test]
    fn a_show_event_wakes_a_waiting_hidden_loop_at_once() {
        use super::bench_seam::IdleLoop;
        use crate::test_support::coverage_relaxed;
        let dir = tempfile::tempdir().unwrap();
        let looped = IdleLoop::start(dir.path(), false).unwrap();
        assert!(until(coverage_relaxed(Duration::from_secs(10)), || {
            looped.rounds() >= 1
        }));
        // Let it get into the wait.
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(looped.passes(), 0);
        let shown = Instant::now();
        looped.set_visible(true);
        assert!(
            until(coverage_relaxed(Duration::from_millis(50)), || {
                looped.passes() >= 1
            }),
            "no pass {:?} after the show event",
            shown.elapsed()
        );
        looped.stop();
    }

    #[test]
    fn closing_the_host_wakes_a_waiting_hidden_loop() {
        use super::bench_seam::IdleLoop;
        use crate::test_support::coverage_relaxed;
        let dir = tempfile::tempdir().unwrap();
        let looped = IdleLoop::start(dir.path(), false).unwrap();
        assert!(until(coverage_relaxed(Duration::from_secs(10)), || {
            looped.rounds() >= 1
        }));
        std::thread::sleep(Duration::from_millis(100));
        let stopping = Instant::now();
        looped.stop();
        assert!(
            stopping.elapsed() < coverage_relaxed(Duration::from_millis(50)),
            "the loop took {:?} to notice the close",
            stopping.elapsed()
        );
    }

    /// A host whose close lands right after the loop was told it was open.
    struct Closing<'a> {
        beacon: &'a Beacon,
        asked: Cell<u32>,
        blocked: Cell<u32>,
    }

    impl Env for Closing<'_> {
        fn is_open(&self) -> bool {
            let asked = self.asked.get();
            self.asked.set(asked + 1);
            if asked == 0 {
                // `shutdown` sets the flag and wakes, in that order; the loop
                // had already been told "open".
                self.beacon.wake();
                return true;
            }
            false
        }
        fn now(&self) -> Instant {
            Instant::now()
        }
        fn retention_tick(&mut self, _now: Instant) {}
        fn retention_next_in(&self, _now: Instant) -> Duration {
            Duration::from_secs(600)
        }
        fn query_window(&self) -> bool {
            false
        }
        fn poll(&mut self) -> Result<bool, String> {
            Ok(false)
        }
        fn announce(&self) -> Result<(), String> {
            Ok(())
        }
        fn wait(&mut self, beacon: &Beacon, seen: u64, _timeout: Duration) {
            if beacon.generation() == seen {
                self.blocked.set(self.blocked.get() + 1);
            }
        }
    }

    /// A close that lands between the loop's "is the host open" and its read
    /// of the counter must still end the wait it goes on to make.
    #[test]
    fn a_close_after_the_open_check_does_not_leave_the_loop_waiting() {
        let beacon = Beacon::new(false);
        let mut env = Closing {
            beacon: &beacon,
            asked: Cell::new(0),
            blocked: Cell::new(0),
        };
        super::drive(&mut env, &beacon);
        assert_eq!(
            env.blocked.get(),
            0,
            "the loop went to sleep on a host that had already closed"
        );
    }

    #[test]
    fn the_counter_wraps_without_losing_a_wakeup() {
        let beacon = Beacon::new(false);
        *beacon.generation.lock().unwrap() = u64::MAX;
        let seen = beacon.generation();
        beacon.wake();
        assert_eq!(beacon.generation(), 0);
        assert!(beacon.wait(seen, Duration::from_secs(60)));
    }

    #[test]
    fn a_poisoned_beacon_still_wakes_and_waits() {
        let beacon = std::sync::Arc::new(Beacon::new(false));
        let poisoner = std::sync::Arc::clone(&beacon);
        let _ = std::thread::spawn(move || {
            let _held = poisoner.generation.lock().unwrap();
            panic!("poison the beacon");
        })
        .join();
        assert!(beacon.generation.is_poisoned());
        let seen = beacon.generation();
        assert!(!beacon.wait(seen, Duration::from_millis(5)));
        beacon.set_on_screen(true);
        assert!(beacon.wait(seen, Duration::from_secs(60)));
        assert!(beacon.on_screen());
        beacon.wake();
    }

    #[test]
    fn a_zero_and_an_unbounded_wait_are_both_safe() {
        let beacon = std::sync::Arc::new(Beacon::new(false));
        let seen = beacon.generation();
        let started = Instant::now();
        assert!(!beacon.wait(seen, Duration::ZERO));
        assert!(started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(1)));
        let waker = std::sync::Arc::clone(&beacon);
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            waker.wake();
        });
        assert!(
            beacon.wait(seen, Duration::MAX),
            "an unbounded wait ends on a wake"
        );
        thread.join().unwrap();
    }

    /// Whatever the window does, the loop's rounds are bounded by the clock and
    /// by events: a simulated hour hidden is about two a minute, not a spin.
    #[test]
    fn an_hour_hidden_is_about_two_rounds_a_minute() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(3600));
        super::drive(&mut sim, &beacon);
        assert!(
            sim.waits.len() <= 125,
            "{} rounds in a simulated hour hidden",
            sim.waits.len()
        );
        assert!(sim.waits.iter().all(|wait| *wait >= super::MIN_WAIT));
        assert_eq!(sim.polls, 0);
    }

    /// The real window says visible while the events say hidden, over and
    /// over: the disagreement costs passes at the visible rate, no more.
    #[test]
    fn a_window_that_disagrees_with_its_events_costs_no_more_than_the_visible_rate() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, true, Duration::from_secs(3600));
        for minute in 1..60 {
            sim.events.push((Duration::from_secs(60 * minute), false));
        }
        super::drive(&mut sim, &beacon);
        assert!(sim.polls <= 3600, "{} passes in an hour", sim.polls);
        assert!(
            sim.waits.len() <= 3600 + 60,
            "{} rounds in an hour",
            sim.waits.len()
        );
    }

    /// A clock that says a retention look is always due cannot make the loop
    /// spin faster than the shortest wait.
    #[test]
    fn a_retention_clock_that_is_always_due_is_held_to_the_shortest_wait() {
        let beacon = Beacon::new(false);
        let sim = Sim::new(&beacon, false, Duration::from_secs(60));
        // Never records a look, so `retention_next_in` stays zero.
        struct NeverLooked<'a>(Sim<'a>);
        impl Env for NeverLooked<'_> {
            fn is_open(&self) -> bool {
                self.0.is_open()
            }
            fn now(&self) -> Instant {
                self.0.now()
            }
            fn retention_tick(&mut self, _now: Instant) {}
            fn retention_next_in(&self, _now: Instant) -> Duration {
                Duration::ZERO
            }
            fn query_window(&self) -> bool {
                false
            }
            fn poll(&mut self) -> Result<bool, String> {
                self.0.poll()
            }
            fn announce(&self) -> Result<(), String> {
                Ok(())
            }
            fn wait(&mut self, beacon: &Beacon, seen: u64, timeout: Duration) {
                self.0.wait(beacon, seen, timeout);
            }
        }
        let mut env = NeverLooked(sim);
        super::drive(&mut env, &beacon);
        assert!(
            env.0.waits.iter().all(|wait| *wait >= super::MIN_WAIT),
            "a wait shorter than the floor: {:?}",
            env.0.waits.iter().min()
        );
        assert!(env.0.waits.len() <= 60_000 + 1, "{}", env.0.waits.len());
    }

    /// A day hidden: retention looks exactly as often as it did when the loop
    /// woke every two seconds, once a window.
    #[test]
    fn retention_looks_once_a_window_over_a_simulated_day_hidden() {
        let beacon = Beacon::new(false);
        let mut sim = Sim::new(&beacon, false, Duration::from_secs(24 * 3600));
        super::drive(&mut sim, &beacon);
        assert_eq!(sim.retention_checks, 144);
        assert_eq!(sim.polls, 0);
    }

    fn profile_in(dir: &std::path::Path) -> std::path::PathBuf {
        let path = dir.join("workbench.sqlite");
        write(&path, 1);
        path
    }

    /// An external write shows within about a second while visible.
    #[test]
    fn an_external_write_while_visible_is_announced_within_a_second() {
        let dir = tempfile::tempdir().unwrap();
        let beacon = Beacon::new(true);
        let mut sim = Sim::new(&beacon, true, Duration::from_secs(30));
        sim.watch = Some((profile_in(dir.path()), Watcher::default()));
        sim.writes.push((Duration::from_millis(7_300), 2));
        super::drive(&mut sim, &beacon);
        let announced = sim.announced.borrow().clone();
        assert_eq!(announced.len(), 1, "{announced:?}");
        assert!(
            announced[0] >= Duration::from_millis(7_300)
                && announced[0] <= Duration::from_millis(7_300) + VISIBLE_PERIOD,
            "announced at {:?}",
            announced[0]
        );
    }

    /// Visible, then hidden, then visible: a commit made while hidden is
    /// announced by the first pass after the show, not lost and not late.
    #[test]
    fn a_write_made_while_hidden_is_announced_on_the_show() {
        let dir = tempfile::tempdir().unwrap();
        let beacon = Beacon::new(true);
        let mut sim = Sim::new(&beacon, true, Duration::from_secs(60));
        sim.watch = Some((profile_in(dir.path()), Watcher::default()));
        sim.events.push((Duration::from_secs(5), false));
        sim.events.push((Duration::from_secs(40), true));
        sim.writes.push((Duration::from_secs(20), 2));
        super::drive(&mut sim, &beacon);
        assert_eq!(
            *sim.announced.borrow(),
            vec![Duration::from_secs(40)],
            "one announcement, at the show"
        );
    }

    /// Show, hide, show inside a millisecond, and a burst of them: the last
    /// event decides, and nothing waits on the earlier ones.
    #[test]
    fn a_burst_of_window_events_settles_on_the_last_one() {
        let beacon = Beacon::new(false);
        let seen = beacon.generation();
        for on in [true, false, true, false, true] {
            beacon.set_on_screen(on);
        }
        assert!(beacon.on_screen());
        assert!(beacon.wait(seen, Duration::from_secs(60)));
        let seen = beacon.generation();
        beacon.set_on_screen(false);
        beacon.set_on_screen(true);
        // A real query that read the state before those events is older.
        beacon.correct(seen, false);
        assert!(beacon.on_screen(), "a stale query overwrote a newer event");
    }

    /// Threaded: events hammered from another thread while the loop runs never
    /// leave it asleep through a final show.
    #[test]
    fn hide_then_show_in_a_millisecond_never_strands_a_hidden_loop() {
        use super::bench_seam::IdleLoop;
        use crate::test_support::coverage_relaxed;
        let dir = tempfile::tempdir().unwrap();
        let looped = IdleLoop::start(dir.path(), true).unwrap();
        for _ in 0..200 {
            looped.set_visible(false);
            looped.set_visible(true);
            std::thread::sleep(Duration::from_millis(1));
        }
        looped.set_visible(false);
        assert!(until(coverage_relaxed(Duration::from_secs(10)), || {
            looped.rounds() >= 1
        }));
        std::thread::sleep(Duration::from_millis(50));
        let before = looped.passes();
        looped.set_visible(true);
        assert!(
            until(coverage_relaxed(Duration::from_millis(50)), || {
                looped.passes() > before
            }),
            "the show after the burst was slept through"
        );
        looped.stop();
    }
}
