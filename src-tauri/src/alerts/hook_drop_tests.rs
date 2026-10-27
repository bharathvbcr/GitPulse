//! The app's half of the durable record of notify replies the hook stopped
//! waiting for (ft-6d8ff969, item 4): what `status()` reads back, and the
//! bounds it keeps the record under.
//!
//! The reader is driven with its clock and its socket path named, so no test
//! depends on the host's configuration directory or on when it runs.

use super::*;
use crate::hooks::notify_drops::{
    prev_path, read_record, record_path, READ_CAP_BYTES, ROTATE_BYTES, WINDOW_MS,
};
use std::path::{Path, PathBuf};

const NOW: u64 = 1_790_000_000_000;
const HOUR: u64 = 60 * 60 * 1000;

fn socket_in(dir: &Path) -> PathBuf {
    dir.join("agent-notify.sock")
}

fn line(at: u64, event: &str) -> String {
    format!("{{\"v\":1,\"at\":{at},\"event\":\"{event}\",\"session\":\"term-1-1\"}}\n")
}

fn write_record(socket: &Path, text: &str) {
    std::fs::write(record_path(socket), text).unwrap();
}

fn bound(socket: &Path) -> bridge::BridgeStatus {
    bridge::BridgeStatus {
        listening: true,
        path: Some(socket.display().to_string()),
        error: None,
    }
}

/// A record with one of each shape the reader must decide about.
fn mixed_fixture() -> String {
    let mut text = String::new();
    text.push_str(&line(NOW - 2 * HOUR, "tool_finished"));
    text.push_str(&line(NOW - HOUR, "session_ended"));
    // Torn: the hook was killed mid-write, or a reader met a half-flushed tail.
    text.push_str("{\"v\":1,\"at\":17");
    text.push('\n');
    // Malformed: not this module's line at all, and still not nothing.
    text.push_str("this is not json\n");
    // No time: counted as current.
    text.push_str("{\"v\":1,\"event\":\"idle_prompt\"}\n");
    // Older than the window.
    text.push_str(&line(NOW - WINDOW_MS - HOUR, "permission_prompt"));
    // Blank.
    text.push_str("\n   \n");
    text
}

#[test]
fn status_counts_a_fixture_with_a_torn_and_a_malformed_line_and_names_the_newest() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    write_record(&socket, &mixed_fixture());
    let status = status_from(bound(&socket), NOW);
    // Two whole lines, one torn, one malformed, one with no time.
    assert_eq!(status.hook_replies_missed, 5);
    assert_eq!(
        status.hook_reply_missed_last.as_deref(),
        Some(&*format!(
            "session_ended at {}",
            crate::ledger::ids::iso8601_utc(NOW - HOUR)
        ))
    );
    assert_eq!(status.hook_drop_record_error, None);
}

#[test]
fn a_line_exactly_seven_days_old_counts_and_one_millisecond_older_does_not() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let mut text = line(NOW - WINDOW_MS, "session_ended");
    text.push_str(&line(NOW - WINDOW_MS - 1, "session_ended"));
    write_record(&socket, &text);
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 1);
    assert_eq!(report.error, None);
}

#[test]
fn an_absent_record_is_zero_with_no_error() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let status = status_from(bound(&socket), NOW);
    assert_eq!(status.hook_replies_missed, 0);
    assert_eq!(status.hook_reply_missed_last, None);
    assert_eq!(status.hook_drop_record_error, None);
}

#[test]
fn the_record_is_read_beside_the_socket_the_bridge_is_bound_to() {
    let bound_dir = tempfile::tempdir().unwrap();
    let other_dir = tempfile::tempdir().unwrap();
    write_record(
        &socket_in(bound_dir.path()),
        &line(NOW - HOUR, "session_ended"),
    );
    write_record(
        &socket_in(other_dir.path()),
        &line(NOW - HOUR, "idle_prompt"),
    );
    let status = status_from(bound(&socket_in(bound_dir.path())), NOW);
    assert_eq!(status.hook_replies_missed, 1);
    assert!(status
        .hook_reply_missed_last
        .unwrap()
        .starts_with("session_ended at "));
}

#[cfg(unix)]
#[test]
fn an_unreadable_record_sets_the_error_and_reads_zero_not_a_clean_zero() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    write_record(&socket, &line(NOW - HOUR, "session_ended"));
    let path = record_path(&socket);
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
    // A superuser reads it anyway; the case is not reachable there.
    if std::fs::read(&path).is_ok() {
        return;
    }
    let status = status_from(bound(&socket), NOW);
    assert_eq!(status.hook_replies_missed, 0);
    let error = status.hook_drop_record_error.expect("unknown is not zero");
    assert!(error.contains("count may be too low"), "{error}");
    assert!(error.contains(&path.display().to_string()), "{error}");
}

#[cfg(unix)]
#[test]
fn a_record_that_is_not_a_regular_file_is_an_error_and_is_never_followed() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, line(NOW - HOUR, "session_ended")).unwrap();
    std::os::unix::fs::symlink(&target, record_path(&socket)).unwrap();
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 0, "a symlink was followed");
    assert!(report.error.is_some());

    let other = tempfile::tempdir().unwrap();
    let socket = socket_in(other.path());
    std::fs::create_dir(record_path(&socket)).unwrap();
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 0);
    assert!(report.error.unwrap().contains("not a regular file"));
}

#[test]
fn a_generation_past_the_read_cap_is_reported_not_counted_from_a_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    // Over the rotation size too, so it is renamed first: the cap holds for
    // `.prev` as it does for the current file.
    let one = line(NOW - HOUR, "session_ended");
    let big = one.repeat(READ_CAP_BYTES as usize / one.len() + 10);
    assert!(big.len() as u64 > READ_CAP_BYTES);
    write_record(&socket, &big);
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 0);
    let error = report.error.expect("over the cap is an error");
    assert!(error.contains("KiB the reader takes"), "{error}");
}

#[test]
fn both_generations_are_read_and_the_newest_line_wins_whichever_file_holds_it() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let mut current = line(NOW - 3 * HOUR, "idle_prompt");
    current.push_str(&line(NOW - 2 * HOUR, "idle_prompt"));
    write_record(&socket, &current);
    std::fs::write(
        prev_path(&record_path(&socket)),
        line(NOW - HOUR, "session_ended"),
    )
    .unwrap();
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 3);
    assert!(report.last.unwrap().starts_with("session_ended at "));
}

#[test]
fn a_record_over_32_kib_is_renamed_to_prev_replacing_the_old_one_and_never_truncated() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let one = line(NOW - HOUR, "session_ended");
    let count = ROTATE_BYTES as usize / one.len() + 5;
    let big = one.repeat(count);
    assert!(big.len() as u64 > ROTATE_BYTES);
    write_record(&socket, &big);
    let previous = prev_path(&record_path(&socket));
    std::fs::write(&previous, line(NOW - 2 * HOUR, "idle_prompt")).unwrap();

    let report = read_record(&socket, NOW);

    assert!(
        !record_path(&socket).exists(),
        "the current file was left in place"
    );
    assert_eq!(
        std::fs::read_to_string(&previous).unwrap(),
        big,
        "the old generation was not replaced by the whole of the renamed one"
    );
    // Both generations were read: nothing was lost to the rename, and the
    // replaced `.prev` is gone by design (two generations by size).
    assert_eq!(report.missed, count as u64);
    assert_eq!(report.error, None);

    // A hook that appends afterwards starts a new current file; the next read
    // sees the old one and the new line.
    write_record(&socket, &line(NOW - HOUR / 2, "tool_finished"));
    assert_eq!(read_record(&socket, NOW).missed, count as u64 + 1);
}

#[test]
fn a_record_of_exactly_32_kib_is_left_where_it_is() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let one = line(NOW - HOUR, "session_ended");
    let mut text = one.repeat(ROTATE_BYTES as usize / one.len());
    // Pad with blank lines to the size exactly.
    text.push_str(&"\n".repeat(ROTATE_BYTES as usize - text.len()));
    assert_eq!(text.len() as u64, ROTATE_BYTES);
    write_record(&socket, &text);
    read_record(&socket, NOW);
    assert!(record_path(&socket).exists());
    assert!(!prev_path(&record_path(&socket)).exists());
}

#[test]
fn an_event_the_reader_shows_is_cleaned_and_bounded() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let hostile = format!("a\\u202eb\\u0007{}", "z".repeat(200));
    write_record(&socket, &line(NOW - HOUR, &hostile));
    let last = read_record(&socket, NOW).last.expect("a parsable line");
    let (event, _) = last.split_once(" at ").unwrap();
    assert!(event.chars().count() <= 40, "{event}");
    assert!(
        !event.contains('\u{202e}') && !event.contains('\u{7}'),
        "{event:?}"
    );
}

#[test]
fn the_three_fields_are_on_the_wire_under_the_names_the_store_mirrors() {
    let status = SessionAlertStatus {
        hook_replies_missed: 4,
        hook_reply_missed_last: Some("session_ended at t".into()),
        hook_drop_record_error: Some("e".into()),
        ..Default::default()
    };
    let wire = serde_json::to_value(&status).unwrap();
    assert_eq!(wire["hook_replies_missed"], 4);
    assert_eq!(wire["hook_reply_missed_last"], "session_ended at t");
    assert_eq!(wire["hook_drop_record_error"], "e");
    let quiet = serde_json::to_value(SessionAlertStatus::default()).unwrap();
    assert_eq!(quiet["hook_replies_missed"], 0);
    assert!(quiet["hook_reply_missed_last"].is_null());
    assert!(quiet["hook_drop_record_error"].is_null());
}
