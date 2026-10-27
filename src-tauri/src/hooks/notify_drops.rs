//! `notify` reports whose reply missed its bound.
//!
//! A hook the host is waiting on cannot wait for a socket that is not
//! answering, so `run_notify` gives up at the report's budget and abandons the
//! send (`within_budget`). For `SessionEnd` that budget is 300 ms
//! ([`super::SESSION_END_NOTIFY_BUDGET`]), and the report it carries is the one
//! that clears an approval GitPulse is still showing, so a miss is not
//! harmless. A miss leaves one stderr line in the host's debug log, a count in
//! this process, and one line in a record the app reads.
//!
//! The count has the shape `ledger::LedgerStatus` gives its appends:
//! `dropped`, and a `dropped_scope` saying whose count it is. It mirrors that
//! shape rather than extending the ledger's own, because these are not ledger
//! appends and the ledger snapshot is read by surfaces that mean exactly that.
//! Its scope is the ledger's: `this_process`. A hook is a one-shot process, so
//! a nonzero count is something only that run can say, and a reader's 0 is not
//! a verdict on any other.
//!
//! The record is what carries a miss to a person. `NotifyDrops` is the owner of
//! both halves of its format:
//!
//! * the hook appends one line per miss to `<socket path>.drops`, beside the
//!   socket it resolved (`GITPULSE_NOTIFY_SOCKET` redirects the record too):
//!   `{"v":1,"at":<unix ms>,"event":"<name>","session":"<key>"}`. Event names
//!   and a time, never payload text; `at` and `session` are left out when
//!   unknown. The sink belongs to [`NotifyDrops`]: [`NotifyDrops::new`] has
//!   none and never touches disk, [`NotifyDrops::recording`] carries one, and
//!   only the process's own count ([`PROCESS_DROPS`]) is built with it.
//! * the app reads it back ([`read_record`], called by `alerts::status`) and
//!   is the only writer that ever shortens it: past [`ROTATE_BYTES`] it renames
//!   the file to `<name>.prev`, replacing the older generation, so an append in
//!   flight lands in the renamed file and no line is lost. Two generations by
//!   size; the count shows lines at most [`WINDOW_MS`] old. Rotation has one
//!   owner at a time across processes (a non-blocking `flock` on
//!   `<record>.lock`, size checked again under it); a reader that does not get
//!   the lock reads without rotating. The hook's own backstop is
//!   [`HOOK_STOP_BYTES`]: at or over it the append is skipped and the stderr
//!   line says so, so a record nobody rotates stays bounded. A generation at
//!   that size means later misses were not recorded, and the reader says the
//!   count is a lower bound rather than showing it as complete.
//!
//! The record is a user-private file (mode 0600, no secrets, no payload) that
//! only the hook writes and only the app reads. Nothing here is a transport:
//! a miss is a fact about the hook's wait, not a delivery verdict, which is why
//! the app's line about it never says "lost".

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// What the record's file name adds to the socket's.
const RECORD_SUFFIX: &str = ".drops";
/// What the app's rotation adds to the record's name.
const PREV_SUFFIX: &str = ".prev";
/// What the record's name adds for the file the app's rotation locks.
const LOCK_SUFFIX: &str = ".lock";
/// At or over this the hook does not append. The app rotates long before it.
pub(crate) const HOOK_STOP_BYTES: u64 = 64 * 1024;
/// Over this the app renames the record to `.prev` before it reads.
pub(crate) const ROTATE_BYTES: u64 = 32 * 1024;
/// The most of one generation the app will read. Both stay under
/// [`HOOK_STOP_BYTES`] by construction; a file above this was not written by
/// the hook, and is reported rather than counted from a prefix.
pub(crate) const READ_CAP_BYTES: u64 = 256 * 1024;
/// How far back the app counts.
pub(crate) const WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
/// A line the hook writes is shorter than this, so one `write()` of it is one
/// atomic append.
const MAX_LINE_BYTES: usize = 200;
/// The longest session key the hook records. Longer is left out, not cut: a
/// clipped key names a different session.
const MAX_SESSION_BYTES: usize = 64;
/// The longest event name the app shows from a line it did not write.
const MAX_SHOWN_EVENT_CHARS: usize = 40;

/// The count and whose it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DropSnapshot {
    /// Reports this process gave up on for want of a reply: the outer budget
    /// passed (300 ms for `SessionEnd`, 1.5 s for the other events), the
    /// socket's own deadline passed, or the peer closed without replying. Not
    /// confirmed delivered and not retried; the app may or may not have
    /// received them. A worker that panicked or could not start is not counted.
    pub dropped: u64,
    /// What `dropped` counts: [`crate::ledger::DROPPED_SCOPE`].
    pub dropped_scope: String,
}

/// Where a miss is written, and the clock that stamps it. Both are the sink's
/// to be given, so a test names its own time; the path comes from the socket
/// the hook resolved, never from here.
#[derive(Debug, Clone, Copy)]
pub struct DropRecord {
    /// Unix milliseconds, or `None` when the clock cannot be read: the line
    /// then carries no `at`, which the app counts as current.
    now_ms: fn() -> Option<u64>,
}

impl DropRecord {
    /// The record a hook process writes, stamped by the wall clock.
    pub const fn on_disk() -> Self {
        DropRecord { now_ms: wall_clock }
    }

    /// The same record, stamped by `now_ms`.
    #[cfg(test)]
    pub(crate) const fn with_clock(now_ms: fn() -> Option<u64>) -> Self {
        DropRecord { now_ms }
    }

    /// Appends one line for a miss of `event`, beside `socket`. `Err` is the
    /// reason it was not recorded, for the stderr line.
    ///
    /// One `write()` of a line under [`MAX_LINE_BYTES`] to a file opened
    /// `O_APPEND`, so concurrent hooks interleave whole lines or none. No
    /// `fsync`, no retry and no `create_dir`: this runs inside the hook's
    /// budget, and a record that cannot be written changes nothing else.
    pub fn append(&self, socket: &Path, event: &str, session: Option<&str>) -> Result<(), String> {
        let line = encode_line((self.now_ms)(), event, session)?;
        append_to(&record_path(socket), &line)
    }
}

fn wall_clock() -> Option<u64> {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    u64::try_from(since.as_millis()).ok()
}

/// One line of the record, as written.
#[derive(Serialize)]
struct WrittenLine<'a> {
    v: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    at: Option<u64>,
    event: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    session: Option<&'a str>,
}

/// One line as the app reads it. Both fields are optional: a line this module
/// did not finish writing, or another version wrote, is still a miss.
#[derive(Deserialize)]
struct ReadLine {
    at: Option<u64>,
    event: Option<String>,
}

fn encode_line(at: Option<u64>, event: &str, session: Option<&str>) -> Result<Vec<u8>, String> {
    // A key outside the alphabet GitPulse mints, or a long one, is left out:
    // it keeps the line under its bound and JSON-plain, and the miss is still
    // recorded with its event.
    let session = session
        .filter(|key| key.len() <= MAX_SESSION_BYTES && crate::alerts::bridge::valid_key(key));
    let mut line = serde_json::to_vec(&WrittenLine {
        v: 1,
        at,
        event,
        session,
    })
    .map_err(|error| format!("the record line could not be built: {error}"))?;
    line.push(b'\n');
    if line.len() >= MAX_LINE_BYTES {
        return Err(format!(
            "the record line is {} bytes, past its {MAX_LINE_BYTES}-byte bound",
            line.len()
        ));
    }
    Ok(line)
}

/// `<socket>.drops`.
pub(crate) fn record_path(socket: &Path) -> PathBuf {
    let mut name = socket.as_os_str().to_owned();
    name.push(RECORD_SUFFIX);
    PathBuf::from(name)
}

pub(crate) fn prev_path(record: &Path) -> PathBuf {
    let mut name = record.as_os_str().to_owned();
    name.push(PREV_SUFFIX);
    PathBuf::from(name)
}

/// The file whose lock makes rotation single-owner.
pub(crate) fn lock_path(record: &Path) -> PathBuf {
    let mut name = record.as_os_str().to_owned();
    name.push(LOCK_SUFFIX);
    PathBuf::from(name)
}

#[cfg(unix)]
fn append_to(path: &Path, line: &[u8]) -> Result<(), String> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    // `O_NOFOLLOW`: a symlink planted at the record's name is not written
    // through. `O_NONBLOCK`: a FIFO there must not hold the hook's thread in
    // `open`; it is inert for the regular file this is meant to be.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|error| match error.raw_os_error() {
            Some(libc::ELOOP) => format!("{} is a symbolic link", path.display()),
            _ if error.kind() == std::io::ErrorKind::NotFound => {
                format!("{}: its directory does not exist", path.display())
            }
            _ => format!("{}: {error}", path.display()),
        })?;
    // On the open handle, so what was checked is what is written to.
    let meta = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() >= HOOK_STOP_BYTES {
        return Err(format!(
            "{} is at its {} KiB cap",
            path.display(),
            HOOK_STOP_BYTES / 1024
        ));
    }
    match file.write(line) {
        Ok(written) if written == line.len() => Ok(()),
        Ok(written) => Err(format!(
            "{}: short write ({written} of {} bytes)",
            path.display(),
            line.len()
        )),
        Err(error) => Err(format!("{}: {error}", path.display())),
    }
}

/// A platform with no socket has no misses to record; the signature stays so
/// the caller is one code path.
#[cfg(not(unix))]
fn append_to(path: &Path, _line: &[u8]) -> Result<(), String> {
    Err(format!(
        "{}: this platform has no notification socket",
        path.display()
    ))
}

/// What the app reads out of the record.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct DropReport {
    /// Non-empty lines at most [`WINDOW_MS`] old. A torn or malformed line is
    /// one, and so is a line with no parsable time.
    pub missed: u64,
    /// The newest parsable line, as `<event> at <UTC time>`.
    pub last: Option<String>,
    /// Why the record could not be read in full. `missed` is then a floor, never
    /// a verdict: an absent file is 0 with no error, an unreadable one is not.
    pub error: Option<String>,
}

/// Reads the record beside `socket`, rotating it first when it has outgrown
/// [`ROTATE_BYTES`]. `now_ms` is the reader's clock.
///
/// The app never truncates: rotation is a rename, which is atomic, and a hook
/// that has the file open keeps appending to the renamed one.
pub(crate) fn read_record(socket: &Path, now_ms: u64) -> DropReport {
    let current = record_path(socket);
    let previous = prev_path(&current);
    let mut notes = Vec::new();
    if let Err(reason) = rotate_if_large(&current, &previous) {
        notes.push(reason);
    }
    let cutoff = now_ms.saturating_sub(WINDOW_MS);
    let mut report = DropReport::default();
    let mut newest: Option<(u64, String)> = None;
    let mut capped = false;
    for path in [&current, &previous] {
        match read_generation(path) {
            Ok(None) => {}
            Ok(Some(bytes)) => {
                // The hook stops appending here, so every miss after it went
                // unrecorded: what is counted is a floor, not the number.
                capped |= bytes.len() as u64 >= HOOK_STOP_BYTES;
                tally(
                    &String::from_utf8_lossy(&bytes),
                    cutoff,
                    &mut report.missed,
                    &mut newest,
                );
            }
            Err(reason) => notes.push(reason),
        }
    }
    report.last =
        newest.map(|(at, event)| format!("{event} at {}", crate::ledger::ids::iso8601_utc(at)));
    let mut said = Vec::new();
    if !notes.is_empty() {
        said.push(format!(
            "The record of notify reports that got no reply could not be read in full ({}); the count may be too low.",
            notes.join("; ")
        ));
    }
    if capped {
        said.push(format!(
            "The record reached its {} KiB cap; later misses were not recorded, so the count is a lower bound.",
            HOOK_STOP_BYTES / 1024
        ));
    }
    if !said.is_empty() {
        report.error = Some(said.join(" "));
    }
    report
}

fn tally(text: &str, cutoff: u64, missed: &mut u64, newest: &mut Option<(u64, String)>) {
    for raw in text.lines() {
        if raw.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<ReadLine>(raw).ok();
        let at = parsed.as_ref().and_then(|line| line.at);
        // Old enough to leave the window, and only that: a line whose time
        // cannot be read is counted as current rather than dropped.
        if at.is_some_and(|at| at < cutoff) {
            continue;
        }
        *missed = missed.saturating_add(1);
        let (Some(at), Some(event)) = (at, parsed.and_then(|line| line.event)) else {
            continue;
        };
        if newest.as_ref().is_some_and(|(seen, _)| *seen >= at) {
            continue;
        }
        let shown: String = crate::alerts::text::sanitize(&event)
            .chars()
            .take(MAX_SHOWN_EVENT_CHARS)
            .collect();
        // A name that cleans to nothing is still a miss, counted above, but
        // is not something to show as the last one: it would hide an older
        // line that reads.
        if shown.trim().is_empty() {
            continue;
        }
        *newest = Some((at, shown));
    }
}

/// Whether `current` is a regular file over [`ROTATE_BYTES`]. Absent is not.
fn needs_rotation(current: &Path) -> Result<bool, String> {
    match std::fs::symlink_metadata(current) {
        Ok(meta) => Ok(meta.is_file() && meta.len() > ROTATE_BYTES),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("{}: {error}", current.display())),
    }
}

/// Renames `current` to `previous` when it is a regular file over
/// [`ROTATE_BYTES`], and only while holding the rotation lock.
///
/// A stat and a rename are two steps, and a second reader (another app
/// instance, or an overlapping `status()`) between them would rename the small
/// file a hook had started in the meantime over `.prev`, destroying the whole
/// first generation. So rotation has one owner at a time: an exclusive,
/// non-blocking `flock` on `<record>.lock`, taken only once the size says a
/// rotation is due, with the size checked again under it. A reader that does
/// not get the lock skips rotation for this poll and still reads. Hooks never
/// take it: they only append, and an append in flight lands in whichever file
/// it opened.
fn rotate_if_large(current: &Path, previous: &Path) -> Result<(), String> {
    if !needs_rotation(current)? {
        return Ok(());
    }
    let lock_path = lock_path(current);
    let lock = crate::fs_entry::open_private_record(&lock_path)?;
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Ok(()),
        Err(std::fs::TryLockError::Error(error)) => {
            return Err(format!("could not lock {}: {error}", lock_path.display()))
        }
    }
    // The owner before us may have rotated it already.
    if !needs_rotation(current)? {
        return Ok(());
    }
    match std::fs::rename(current, previous) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "{} could not be rotated to {}: {error}",
            current.display(),
            previous.display()
        )),
    }
}

/// One generation's bytes; `None` when it is absent.
fn read_generation(path: &Path) -> Result<Option<Vec<u8>>, String> {
    use std::io::Read;
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // As the hook opens it: no symlink followed, no FIFO waited on.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{}: {error}", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }
    if meta.len() > READ_CAP_BYTES {
        return Err(format!(
            "{} is {} bytes, past the {} KiB the reader takes",
            path.display(),
            meta.len(),
            READ_CAP_BYTES / 1024
        ));
    }
    let mut bytes = Vec::new();
    // One past the cap, so a file that grew since the check is still seen as
    // over it rather than read as a prefix.
    file.take(READ_CAP_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("{}: {error}", path.display()))?;
    if bytes.len() as u64 > READ_CAP_BYTES {
        return Err(format!(
            "{} grew past the {} KiB the reader takes",
            path.display(),
            READ_CAP_BYTES / 1024
        ));
    }
    Ok(Some(bytes))
}

/// Counts the reports a process gave up on, and carries the sink that writes
/// each of them down.
#[derive(Debug, Default)]
pub struct NotifyDrops {
    count: AtomicU64,
    record: Option<DropRecord>,
}

impl NotifyDrops {
    /// A count with no sink: it never touches disk. Production builds only the
    /// recording one ([`PROCESS_DROPS`]), so this is for the tests that count
    /// without a record.
    #[cfg(test)]
    pub const fn new() -> Self {
        NotifyDrops {
            count: AtomicU64::new(0),
            record: None,
        }
    }

    /// A count whose every miss is also appended through `record`.
    pub const fn recording(record: DropRecord) -> Self {
        NotifyDrops {
            count: AtomicU64::new(0),
            record: Some(record),
        }
    }

    /// A count that starts at `count`, for a test of the saturation bound.
    #[cfg(test)]
    pub(crate) const fn starting_at(count: u64) -> Self {
        NotifyDrops {
            count: AtomicU64::new(count),
            record: None,
        }
    }

    /// Records one abandoned report and returns the count including it.
    /// The stored count saturates at `u64::MAX`; it never wraps to a smaller
    /// number, which would read as "nothing was dropped".
    pub fn record(&self) -> u64 {
        // A compare-exchange loop rather than `fetch_add`, which wraps.
        let mut stored = self.count.load(Ordering::Relaxed);
        loop {
            let next = stored.saturating_add(1);
            match self.count.compare_exchange_weak(
                stored,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return next,
                Err(actual) => stored = actual,
            }
        }
    }

    /// Writes one abandoned report down beside `socket`. `None` when this
    /// count has no sink; `Some(Err(reason))` when it has one and the line was
    /// not written. The in-memory count is not this function's to change.
    pub fn append_record(
        &self,
        socket: &Path,
        event: &str,
        session: Option<&str>,
    ) -> Option<Result<(), String>> {
        self.record
            .map(|record| record.append(socket, event, session))
    }

    pub fn snapshot(&self) -> DropSnapshot {
        DropSnapshot {
            dropped: self.count.load(Ordering::Relaxed),
            dropped_scope: crate::ledger::DROPPED_SCOPE.to_string(),
        }
    }
}

/// The process's own count, which `dispatch` passes in. It writes each miss to
/// the record. Tests pass their own, which does not unless they say so.
pub(super) static PROCESS_DROPS: NotifyDrops = NotifyDrops::recording(DropRecord::on_disk());
