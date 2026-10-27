//! W4c adversary attacks on the drop record's existing-file tightening
//! (ft-f70ee2bb item 1) and on ctime as the rotation time (item 2).
//!
//! The attacks that broke the first lane tip (1d, 1d', 2c) were fixed in rework
//! 1 and their tests assert the ruled behaviour: tightening is best effort and
//! never a precondition, and the reader leaves a `.prev` alone. Every other test
//! records an attack that held.

use super::*;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

use crate::test_support::env::coverage_relaxed;

const HOUR: u64 = 60 * 60 * 1000;

fn socket_in(dir: &Path) -> PathBuf {
    dir.join("agent-notify.sock")
}

fn line(at: u64, event: &str) -> String {
    format!("{{\"v\":1,\"at\":{at},\"event\":\"{event}\",\"session\":\"term-1-1\"}}\n")
}

fn clock() -> Option<u64> {
    Some(1_790_000_000_000)
}

fn mode_of(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().mode() & 0o7777
}

fn put(path: &Path, text: &[u8], mode: u32) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

fn full_generation(at: u64) -> Vec<u8> {
    let one = line(at, "session_ended");
    one.repeat(HOOK_STOP_BYTES as usize / one.len() + 1)
        .into_bytes()
}

fn wall() -> u64 {
    wall_clock().expect("the host clock reads")
}

fn ctime_ms(path: &Path) -> u64 {
    let meta = std::fs::metadata(path).unwrap();
    status_changed_ms(&meta).expect("ctime")
}

fn lower_bound_said(report: &DropReport) -> bool {
    report
        .error
        .as_deref()
        .is_some_and(|error| error.contains("lower bound"))
}

fn mkfifo(path: &Path, mode: u32) {
    let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: `c` is a valid NUL-terminated path for the call.
    assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), mode as libc::mode_t) }, 0);
    // The mode given to mkfifo is masked by the umask; the test wants it exact.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Sets the BSD file flags on `path`, and clears them again on drop so the
/// tempdir can be removed.
#[cfg(target_os = "macos")]
struct Flagged(PathBuf);

#[cfg(target_os = "macos")]
impl Flagged {
    fn new(path: &Path, flags: u32) -> Self {
        let c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        assert_eq!(unsafe { libc::chflags(c.as_ptr(), flags) }, 0, "chflags");
        Flagged(path.to_path_buf())
    }
}

#[cfg(target_os = "macos")]
impl Drop for Flagged {
    fn drop(&mut self) {
        let c = std::ffi::CString::new(self.0.as_os_str().as_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call.
        unsafe { libc::chflags(c.as_ptr(), 0) };
    }
}

// Attack 1a: TOCTOU. The record's name flips between an owned wide file and a
// symlink to a victim while hooks append. fchmod is on the handle, so the
// victim (0644, ours) must never be narrowed or written.
#[test]
fn adv_a1_a_name_flipping_to_a_symlink_never_carries_the_chmod_or_the_line_to_the_victim() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let victim = dir.path().join("victim");
    put(&victim, b"keep\n", 0o644);
    put(&record, b"", 0o644);
    let stop = AtomicBool::new(false);
    let mut recorded = 0u32;
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let mut n = 0u64;
            let began = std::time::Instant::now();
            // Bounded by time as well, so a panic in the main loop cannot leave
            // this spinning and the scope joining forever.
            let budget = coverage_relaxed(Duration::from_secs(30));
            while !stop.load(Ordering::Acquire) && began.elapsed() < budget {
                let staged = dir.path().join(format!("staged-{n}"));
                if n.is_multiple_of(2) {
                    std::os::unix::fs::symlink(&victim, &staged).unwrap();
                } else {
                    put(&staged, b"", 0o644);
                }
                let _ = std::fs::rename(&staged, &record);
                n += 1;
            }
        });
        let started = std::time::Instant::now();
        while started.elapsed() < coverage_relaxed(Duration::from_secs(3)) {
            let appended = DropRecord::with_clock(clock).append(&socket, "session_ended", None);
            if appended.is_ok() {
                recorded += 1;
            }
            assert_eq!(mode_of(&victim), 0o644, "the victim was narrowed");
        }
        stop.store(true, Ordering::Release);
    });
    assert!(recorded > 0, "the race never let a line through");
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep\n");
    assert_eq!(mode_of(&victim), 0o644);
}

// Attack 1a': a hard link at the record's name is the victim's own inode. The
// hook cannot tell it from a record, so it tightens and appends to it. Same
// user only (another user cannot link a file it cannot write, on a system
// that protects hard links), and the base appended to it as well; the change
// only narrows. Recorded as an observation, not a defect.
#[test]
fn adv_a1_a_hard_link_at_the_record_name_is_treated_as_the_record() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let victim = dir.path().join("victim");
    put(&victim, b"keep\n", 0o644);
    std::fs::hard_link(&victim, record_path(&socket)).unwrap();

    DropRecord::with_clock(clock)
        .append(&socket, "session_ended", None)
        .unwrap();

    assert_eq!(mode_of(&victim), 0o600, "narrowed, never widened");
    assert!(std::fs::read_to_string(&victim)
        .unwrap()
        .starts_with("keep\n"));
}

// Attack 1b: a directory or FIFO at the record path, hook side.
#[test]
fn adv_a1_a_directory_or_fifo_at_the_record_path_is_refused_untouched_and_never_waited_on() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    std::fs::create_dir(&record).unwrap();
    std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o755)).unwrap();
    let started = std::time::Instant::now();
    let refused = DropRecord::with_clock(clock)
        .append(&socket, "session_ended", None)
        .unwrap_err();
    assert!(
        started.elapsed() < coverage_relaxed(Duration::from_secs(2)),
        "{refused}"
    );
    assert_eq!(mode_of(&record), 0o755, "{refused}");
    std::fs::remove_dir(&record).unwrap();

    mkfifo(&record, 0o644);
    let started = std::time::Instant::now();
    let no_reader = DropRecord::with_clock(clock)
        .append(&socket, "session_ended", None)
        .unwrap_err();
    assert!(
        started.elapsed() < coverage_relaxed(Duration::from_secs(2)),
        "{no_reader}"
    );
    assert_eq!(mode_of(&record), 0o644);

    // With a reader attached the open succeeds; it is not a regular file.
    let reader = {
        use std::os::unix::fs::OpenOptionsExt;
        std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(&record)
            .unwrap()
    };
    let attached = DropRecord::with_clock(clock)
        .append(&socket, "session_ended", None)
        .unwrap_err();
    assert!(attached.contains("not a regular file"), "{attached}");
    assert_eq!(mode_of(&record), 0o644, "a FIFO is not chmod'ed");
    drop(reader);
}

// Attack 1b': the same, reader side, for the current record and `.prev`.
#[test]
fn adv_a1_a_directory_or_fifo_for_either_generation_is_reported_untouched_and_never_waited_on() {
    for use_fifo in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(dir.path());
        let record = record_path(&socket);
        let previous = prev_path(&record);
        for path in [&record, &previous] {
            if use_fifo {
                mkfifo(path, 0o644);
            } else {
                std::fs::create_dir(path).unwrap();
                std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        let started = std::time::Instant::now();
        let report = read_record(&socket, wall());
        assert!(started.elapsed() < coverage_relaxed(Duration::from_secs(2)));
        let error = report.error.expect("an unreadable record is reported");
        assert!(error.contains("not a regular file"), "{error}");
        assert_eq!(report.missed, 0);
        assert_eq!(mode_of(&record), if use_fifo { 0o644 } else { 0o755 });
        assert_eq!(mode_of(&previous), if use_fifo { 0o644 } else { 0o755 });
    }
}

// Attack 1c: a record this user does not own. The reader must not try to
// chmod it (it would fail and withhold the count), and must still read it.
// `/etc/hosts` is root's and 0644 on macOS and Linux. CONDITIONAL: a host where
// it is missing, owned by this user or not readable by others makes this test a
// no-op, so it is verified on the machine that ran it, not on every runner.
#[test]
fn adv_a1_a_generation_another_user_owns_is_read_and_left_alone_by_the_reader() {
    let hosts = Path::new("/etc/hosts");
    let Ok(meta) = std::fs::metadata(hosts) else {
        return;
    };
    // SAFETY: geteuid has no preconditions and cannot fail.
    if meta.uid() == unsafe { libc::geteuid() } || meta.mode() & 0o044 == 0 {
        return;
    }
    let before = (meta.mode(), meta.ctime(), meta.ctime_nsec());
    let generation = read_generation(hosts, true)
        .expect("readable")
        .expect("present");
    assert_eq!(
        generation.wide, None,
        "not ours, so nothing to say or change"
    );
    assert!(!generation.bytes.is_empty());
    let after = std::fs::metadata(hosts).unwrap();
    assert_eq!(before, (after.mode(), after.ctime(), after.ctime_nsec()));
}

// Attack 1d: fchmod fails on a record the hook can still append to. macOS
// `uappnd` (user append-only) allows `O_APPEND` writes and refuses chmod, and
// an append-only audit log is the legitimate reason anyone sets it. Ruled in
// rework 1: tightening is best effort, so the line is written and counted as
// recorded, the file stays as wide as it was, and the reader says so.
#[cfg(target_os = "macos")]
#[test]
fn adv_a1_a_wide_append_only_record_still_takes_the_line_and_the_reader_says_it_is_wide() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    put(&record, b"", 0o644);
    let _flag = Flagged::new(&record, libc::UF_APPEND);

    let appended = DropRecord::with_clock(clock).append(&socket, "session_ended", None);

    assert_eq!(appended, Ok(()), "the line could have been written");
    assert_eq!(std::fs::read_to_string(&record).unwrap().lines().count(), 1);
    assert_eq!(mode_of(&record), 0o644, "the system refused the narrowing");
    let report = read_record(&socket, clock().unwrap());
    assert_eq!(report.missed, 1, "{:?}", report.error);
    let error = report.error.expect("a wide record is said, not hidden");
    assert!(
        error.contains("readable by other users (mode 0644)"),
        "{error}"
    );
    assert!(error.contains("could not be tightened"), "{error}");
}

// Attack 1d': the same fchmod failure, reader side. The file reads fine and
// its lines are counted whatever happens to the chmod.
#[cfg(target_os = "macos")]
#[test]
fn adv_a1_a_wide_generation_whose_chmod_is_refused_still_counts_its_lines() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let now = wall();
    put(&record, line(now - HOUR, "session_ended").as_bytes(), 0o644);
    let _flag = Flagged::new(&record, libc::UF_APPEND);

    let report = read_record(&socket, now);

    assert_eq!(report.missed, 1, "{:?}", report.error);
}

// Attack 1d'': what the failure reports (so that a wide record is not silent):
// its own note, distinct from the unreadable text, and no "count may be too
// low" because nothing was withheld.
#[cfg(target_os = "macos")]
#[test]
fn adv_a1_a_refused_chmod_on_the_reader_is_its_own_note_and_not_an_unreadable_record() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let now = wall();
    put(&record, line(now - HOUR, "session_ended").as_bytes(), 0o644);
    let _flag = Flagged::new(&record, libc::UF_APPEND);

    let report = read_record(&socket, now);

    let error = report.error.expect("reported");
    assert!(error.contains("could not be tightened"), "{error}");
    assert!(!error.contains("count may be too low"), "{error}");
    assert!(!error.contains("could not be read in full"), "{error}");
}

// Attack 1e: `.lock` and `.prev`. A wide `.lock` is refused, not tightened:
// rotation never happens, the record grows to the hook's cap, and the reader
// says so each poll (loud), and the lower bound once capped.
#[test]
fn adv_a1_a_wide_lock_stops_rotation_loudly_and_a_capped_record_is_then_a_lower_bound() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let now = wall();
    std::fs::write(&record, full_generation(now - HOUR)).unwrap();
    put(&lock_path(&record), b"", 0o644);

    let report = read_record(&socket, now);

    assert!(record.exists(), "no rotation under a wide lock");
    let error = report.error.clone().expect("loud");
    assert!(error.contains("could not be read in full"), "{error}");
    assert!(lower_bound_said(&report), "{error}");
    assert_eq!(
        mode_of(&lock_path(&record)),
        0o644,
        "the lock is not ours to widen or narrow here"
    );
}

// Attack 2a: the exact boundary against a real file's ctime. A rotation at
// the cutoff is inside; one millisecond later it is outside.
#[test]
fn adv_a2_the_seven_day_boundary_on_a_real_ctime_is_inclusive_and_exact() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(&previous, &full_generation(1), 0o600);
    let rotated = ctime_ms(&previous);

    let at_boundary = read_record(&socket, rotated + WINDOW_MS);
    assert!(lower_bound_said(&at_boundary), "{:?}", at_boundary.error);
    let past = read_record(&socket, rotated + WINDOW_MS + 1);
    assert_eq!(past.error, None);
    let before = read_record(&socket, rotated + WINDOW_MS - 1);
    assert!(lower_bound_said(&before));
}

// Attack 2b: anything that changes ctime after the rotation (chmod to the
// same mode, utimes, a second hard link, an xattr from a backup tool) moves
// the "rotation" later. That only extends the note: conservative, never an
// early expiry, but the note then outlives what it is about by up to a week.
#[test]
fn adv_a2_a_status_change_after_rotation_revives_the_note_and_never_ends_it_early() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(&previous, &full_generation(1), 0o600);
    let rotated = ctime_ms(&previous);
    let now = rotated + WINDOW_MS + 20;
    assert_eq!(read_record(&socket, now).error, None, "expired");

    std::thread::sleep(Duration::from_millis(100));
    std::fs::set_permissions(&previous, std::fs::Permissions::from_mode(0o600)).unwrap();
    assert!(ctime_ms(&previous) > rotated + 20, "chmod moved the ctime");

    assert!(
        lower_bound_said(&read_record(&socket, now)),
        "an expired note came back"
    );
}

// Attack 2c: the reader's own tightening is a status change. Ruled in rework 1:
// the reader leaves a `.prev` alone, so a legacy wide capped one expires on its
// first read and stays expired on every later one. It is said to be wide
// (its own note) while it exists, and that note is not the lower-bound one.
#[test]
fn adv_a2_a_wide_capped_prev_that_expired_stays_expired_because_the_reader_leaves_it_alone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(&previous, &full_generation(1), 0o644);
    let rotated = ctime_ms(&previous);
    std::thread::sleep(Duration::from_millis(100));
    let now = rotated + WINDOW_MS + 20;

    let first = read_record(&socket, now);
    let second = read_record(&socket, now);

    assert_eq!(
        mode_of(&previous),
        0o644,
        "a `.prev` is not the reader's to chmod"
    );
    assert_eq!(
        ctime_ms(&previous),
        rotated,
        "so its rotation clock did not move"
    );
    for report in [&first, &second] {
        assert!(!lower_bound_said(report), "{:?}", report.error);
        let error = report.error.as_deref().expect("a wide .prev is said");
        assert!(
            error.contains("readable by other users (mode 0644)"),
            "{error}"
        );
    }
}

// Attack 2d: a clock the file system and the reader disagree on. A ctime in
// the reader's future (clock stepped back after the rotation) keeps the note;
// a reader whose clock is far ahead ends it, consistent with its lines aging
// out of the window by the same clock.
#[test]
fn adv_a2_a_ctime_in_the_readers_future_keeps_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(&previous, &full_generation(1), 0o600);
    let rotated = ctime_ms(&previous);

    let behind = read_record(&socket, rotated.saturating_sub(30 * WINDOW_MS));
    assert!(lower_bound_said(&behind), "{:?}", behind.error);
}

// Attack 2e: both generations capped. The capped current one (which cannot be
// rotated while another reader holds the lock) says the note however old the
// `.prev` is.
#[test]
fn adv_a2_both_generations_capped_and_the_current_one_stuck_still_says_the_note() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let previous = prev_path(&record);
    put(&previous, &full_generation(1), 0o600);
    put(&record, &full_generation(1), 0o600);
    let held = crate::fs_entry::open_private_record(&lock_path(&record)).unwrap();
    held.try_lock().unwrap();
    let far = ctime_ms(&previous) + 30 * WINDOW_MS;

    let report = read_record(&socket, far);

    assert!(record.exists());
    assert!(lower_bound_said(&report), "{:?}", report.error);
}

// Attack 2f: the pure rule at the edges of the representable range.
#[test]
fn adv_a2_the_rule_at_zero_and_maximum_times() {
    assert!(!gap_can_reach_window(false, Some(0), 1));
    assert!(gap_can_reach_window(false, Some(u64::MAX), u64::MAX));
    assert!(gap_can_reach_window(false, Some(0), 0));
}

// Attack 2g: a `.prev` the hook is still appending to (a hook that opened it
// before the rotation) moves its ctime: a write is a status change too. The
// hook refuses at the cap, so a capped `.prev` is never written, and a
// not-capped one never says the note whatever its ctime.
#[test]
fn adv_a2_a_prev_below_the_cap_never_says_the_note_whatever_its_ctime() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(
        &previous,
        &full_generation(1)[..(HOOK_STOP_BYTES as usize - 400)],
        0o600,
    );
    let report = read_record(&socket, ctime_ms(&previous) + 1);
    assert_eq!(report.error, None);
}
