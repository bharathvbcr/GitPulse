//! Residues of the drop record that wave 3 accepted rather than fixed
//! (ft-f70ee2bb items 1 and 2): a record that already exists keeps the mode it
//! was made with, and the "lower bound" note outlived the window it was about.
//!
//! Unix only: both are about modes and status-change times.

use super::*;
use std::os::unix::fs::{MetadataExt, PermissionsExt};

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

/// A file at `path` with `text`, made with exactly `mode` whatever the umask.
fn put(path: &Path, text: &[u8], mode: u32) {
    std::fs::write(path, text).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
}

/// Enough whole lines to reach the hook's cap, stamped `at`.
fn full_generation(at: u64) -> Vec<u8> {
    let one = line(at, "session_ended");
    one.repeat(HOOK_STOP_BYTES as usize / one.len() + 1)
        .into_bytes()
}

fn wall() -> u64 {
    wall_clock().expect("the host clock reads")
}

fn lower_bound_said(report: &DropReport) -> bool {
    report
        .error
        .as_deref()
        .is_some_and(|error| error.contains("lower bound"))
}

// Item 1: a record that is already there.

#[test]
fn an_existing_record_wider_than_0600_is_tightened_and_still_takes_the_line() {
    for wide in [0o644, 0o666, 0o640, 0o604, 0o660, 0o606] {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(dir.path());
        let record = record_path(&socket);
        let first = line(1, "tool_finished");
        put(&record, first.as_bytes(), wide);

        DropRecord::with_clock(clock)
            .append(&socket, "session_ended", Some("term-1-1"))
            .unwrap();

        assert_eq!(mode_of(&record), 0o600, "was {wide:o}");
        let text = std::fs::read_to_string(&record).unwrap();
        assert!(text.starts_with(&first), "{text}");
        assert_eq!(text.lines().count(), 2, "{text}");
    }
}

#[test]
fn a_record_that_is_not_wider_than_0600_is_left_as_it_is() {
    for narrow in [0o600, 0o700, 0o200] {
        let dir = tempfile::tempdir().unwrap();
        let record = dir.path().join("record");
        put(&record, b"", narrow);
        append_owned_by(&record, b"{}\n", my_uid(&record)).unwrap();
        assert_eq!(mode_of(&record), narrow);
    }
}

fn my_uid(path: &Path) -> u32 {
    std::fs::metadata(path).unwrap().uid()
}

#[test]
fn a_record_another_user_owns_is_not_appended_to_and_not_changed() {
    let dir = tempfile::tempdir().unwrap();
    let record = dir.path().join("record");
    put(&record, b"theirs\n", 0o644);
    let someone_else = my_uid(&record) + 1;

    let refusal = append_owned_by(&record, b"{}\n", someone_else).unwrap_err();

    assert!(refusal.contains("owned by uid"), "{refusal}");
    assert!(refusal.contains(&record.display().to_string()), "{refusal}");
    assert_eq!(std::fs::read(&record).unwrap(), b"theirs\n");
    assert_eq!(
        mode_of(&record),
        0o644,
        "a file that is not ours is not ours to chmod"
    );
}

#[test]
fn tightening_is_on_the_opened_handle_so_a_link_never_carries_it_to_its_target() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let target = dir.path().join("elsewhere");
    put(&target, b"keep\n", 0o644);
    std::os::unix::fs::symlink(&target, record_path(&socket)).unwrap();

    let refusal = DropRecord::with_clock(clock)
        .append(&socket, "session_ended", None)
        .unwrap_err();

    assert!(refusal.contains("symbolic link"), "{refusal}");
    assert_eq!(mode_of(&target), 0o644);
    assert_eq!(std::fs::read(&target).unwrap(), b"keep\n");
}

#[test]
fn the_reader_tightens_a_wide_current_record_and_says_a_wide_prev_it_leaves_alone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let previous = prev_path(&record);
    let now = wall();
    put(&record, line(now - HOUR, "session_ended").as_bytes(), 0o644);
    put(
        &previous,
        line(now - 2 * HOUR, "tool_finished").as_bytes(),
        0o666,
    );

    let report = read_record(&socket, now);

    assert_eq!(report.missed, 2, "both are counted");
    assert_eq!(mode_of(&record), 0o600);
    assert_eq!(
        mode_of(&previous),
        0o666,
        "the rotation clock is not touched"
    );
    let error = report.error.expect("the wide .prev is said");
    assert!(
        error.contains("readable by other users (mode 0666)"),
        "{error}"
    );
    assert!(error.contains(&previous.display().to_string()), "{error}");
    assert!(
        !error.contains("0644"),
        "the current record was tightened: {error}"
    );
    assert!(!error.contains("could not be read in full"), "{error}");
}

#[test]
fn a_wide_current_record_is_tightened_before_it_becomes_a_prev_the_reader_leaves_alone() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let now = wall();
    let one = line(now - HOUR, "session_ended");
    let count = ROTATE_BYTES as usize / one.len() + 5;
    put(&record, one.repeat(count).as_bytes(), 0o644);

    let report = read_record(&socket, now);

    assert!(!record.exists(), "it rotated");
    assert_eq!(mode_of(&prev_path(&record)), 0o600);
    assert_eq!(report.missed, count as u64);
    assert_eq!(report.error, None);
}

#[test]
fn a_wide_prev_stops_being_said_once_it_rotates_away() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    let now = wall();
    put(
        &previous,
        line(now - HOUR, "session_ended").as_bytes(),
        0o644,
    );
    assert!(read_record(&socket, now).error.is_some());

    std::fs::remove_file(&previous).unwrap();

    assert_eq!(read_record(&socket, now).error, None);
}

#[test]
fn a_mode_the_reader_leaves_alone_does_not_make_it_rewrite_the_file() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    put(
        &record,
        line(wall() - HOUR, "session_ended").as_bytes(),
        0o600,
    );
    let before = std::fs::metadata(&record).unwrap();

    read_record(&socket, wall());

    let after = std::fs::metadata(&record).unwrap();
    assert_eq!(
        (before.ctime(), before.ctime_nsec()),
        (after.ctime(), after.ctime_nsec())
    );
}

// Item 2: the lower-bound note and the window.

/// What the reader says about a capped generation the app has just rotated,
/// at three times: while the rotation is inside the window, and once it has
/// left it. The clock is the reader's own, so no test waits a week.
#[test]
fn a_capped_prev_stops_being_called_a_lower_bound_once_its_rotation_left_the_window() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let before = wall();
    std::fs::write(&record, full_generation(before - HOUR)).unwrap();

    let at_rotation = read_record(&socket, before);
    assert!(
        !record.exists() && prev_path(&record).exists(),
        "it rotated"
    );
    assert!(lower_bound_said(&at_rotation), "{:?}", at_rotation.error);
    assert!(at_rotation.missed > 0);

    let after = wall();
    let inside = read_record(&socket, after + WINDOW_MS - 60_000);
    assert!(lower_bound_said(&inside), "{:?}", inside.error);

    let outside = read_record(&socket, after + WINDOW_MS + 60_000);
    assert_eq!(
        outside.error, None,
        "the unrecorded period is older than the window"
    );
    assert_eq!(outside.missed, 0, "its lines are too");
}

#[test]
fn a_capped_current_record_is_a_lower_bound_however_late_it_is_read() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = record_path(&socket);
    let before = wall();
    std::fs::write(&record, full_generation(before - HOUR)).unwrap();
    // Another reader holds the rotation, so this one cannot rename it: the
    // file is still the one hooks are refused by.
    let held = crate::fs_entry::open_private_record(&lock_path(&record)).unwrap();
    held.try_lock().unwrap();

    let report = read_record(&socket, before + 30 * WINDOW_MS);

    assert!(record.exists(), "not rotated without the lock");
    assert!(lower_bound_said(&report), "{:?}", report.error);
}

#[test]
fn a_prev_that_is_not_capped_never_says_it_and_a_gap_that_cannot_be_dated_is_inside() {
    // The rule on its own: no platform time is "may be inside", never "is not".
    assert!(gap_can_reach_window(false, None, u64::MAX));
    assert!(
        gap_can_reach_window(false, Some(10), 10),
        "a rotation at the cutoff is inside"
    );
    assert!(!gap_can_reach_window(false, Some(9), 10));
    assert!(
        gap_can_reach_window(true, Some(0), u64::MAX),
        "a current file is still losing them"
    );

    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let previous = prev_path(&record_path(&socket));
    put(
        &previous,
        line(wall() - HOUR, "session_ended").as_bytes(),
        0o600,
    );
    let report = read_record(&socket, wall() + 30 * WINDOW_MS);
    assert_eq!(report.error, None);
}
