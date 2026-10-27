//! The hook's half of the durable record of notify replies it stopped waiting
//! for (ft-6d8ff969, item 4): one line per miss, appended beside the socket the
//! hook resolved, and nothing for any other outcome.
//!
//! Every test names its own socket in its own directory: the record is written
//! next to the socket, so a shared path would make two tests one another's
//! evidence.

use super::notify_drops::{record_path, DropRecord, NotifyDrops, HOOK_STOP_BYTES};
use super::*;
use std::path::{Path, PathBuf};

const FIXED_AT: u64 = 1_790_000_000_000;

fn fixed_clock() -> Option<u64> {
    Some(FIXED_AT)
}

fn recording() -> NotifyDrops {
    NotifyDrops::recording(DropRecord::with_clock(fixed_clock))
}

fn input() -> HookInput {
    HookInput {
        hook_event_name: "SessionEnd".into(),
        session_id: "claude-abc".into(),
        cwd: "/Users/me/GitPulse".into(),
        ..HookInput::default()
    }
}

fn socket_in(dir: &Path) -> PathBuf {
    dir.join("hook.sock")
}

/// One notify run against `socket`, as a session in a GitPulse terminal would.
fn run(
    event: &str,
    socket: &Path,
    drops: &NotifyDrops,
    send: impl FnOnce(&Path, &str) -> Result<(), SendError> + Send + 'static,
) -> Result<HookOutput, HookError> {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1");
    let socket = socket.to_path_buf();
    run_notify_with(
        Some(event),
        &input(),
        send,
        drops,
        Some(Duration::from_secs(30)),
        true,
        move || Some(socket),
    )
}

fn record_lines(socket: &Path) -> Vec<Value> {
    let text = std::fs::read_to_string(record_path(socket)).expect("the record exists");
    text.lines()
        .map(|line| serde_json::from_str(line).expect("each line is JSON"))
        .collect()
}

#[test]
fn a_missed_reply_appends_exactly_one_parsable_line_with_its_event_and_session() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let drops = recording();
    let error = run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply is not a delivery");
    assert!(error.to_string().contains("got no reply"), "{error}");
    assert!(
        !error.to_string().contains("not recorded"),
        "a recorded miss said it was not: {error}"
    );
    assert_eq!(drops.snapshot().dropped, 1);

    let text = std::fs::read_to_string(record_path(&socket)).unwrap();
    assert!(text.ends_with('\n'), "{text:?}");
    assert!(text.len() < 200, "{} bytes: {text:?}", text.len());
    let lines = record_lines(&socket);
    assert_eq!(lines.len(), 1, "{text:?}");
    assert_eq!(lines[0]["v"], json!(1));
    assert_eq!(lines[0]["at"], json!(FIXED_AT));
    assert_eq!(lines[0]["event"], json!("session_ended"));
    assert_eq!(lines[0]["session"], json!("term-4-1"));
}

#[test]
fn both_clocks_that_end_the_wait_write_a_line_and_the_second_miss_appends() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let drops = recording();
    run("tool_finished", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .unwrap_err();
    let (release, held) = mpsc::channel::<()>();
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1");
    let path = socket.clone();
    run_notify_with(
        Some("tool_finished"),
        &input(),
        move |_, _| {
            let _ = held.recv();
            Ok(())
        },
        &drops,
        Some(Duration::from_millis(20)),
        true,
        move || Some(path),
    )
    .unwrap_err();
    drop(release);
    assert_eq!(drops.snapshot().dropped, 2);
    assert_eq!(record_lines(&socket).len(), 2);
}

#[test]
fn a_timely_reply_a_refusal_and_unsupported_append_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let drops = recording();
    run("session_ended", &socket, &drops, |_, _| Ok(())).expect("delivered");
    run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::Failed("refused".into()))
    })
    .unwrap_err();
    run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::Unsupported)
    })
    .unwrap_err();
    run("session_ended", &socket, &drops, |_, _| {
        panic!("worker fault")
    })
    .unwrap_err();
    assert_eq!(drops.snapshot().dropped, 0);
    assert!(
        !record_path(&socket).exists(),
        "an outcome that is not a missed reply wrote a record"
    );
}

#[test]
fn a_transport_that_is_not_there_resolves_no_path_and_writes_no_record() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1");
    let error = run_notify_with(
        Some("session_ended"),
        &input(),
        |_, _| panic!("nothing is sent"),
        &recording(),
        None,
        false,
        || panic!("the path was resolved"),
    )
    .unwrap_err();
    assert!(matches!(error, HookError::Unsupported(_)), "{error:?}");
    assert!(!record_path(&socket).exists());
}

#[test]
fn a_count_made_with_new_never_creates_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let drops = NotifyDrops::new();
    run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .unwrap_err();
    assert_eq!(drops.snapshot().dropped, 1);
    assert!(drops
        .append_record(&socket, "session_ended", None)
        .is_none());
    let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
    assert!(
        entries.is_empty(),
        "NotifyDrops::new() touched disk: {entries:?}"
    );
}

/// The same `Err` and the same count with or without a record, whatever kept
/// the record from being written; the stderr line names which.
fn assert_unrecorded(error: &HookError, drops: &NotifyDrops, reason: &str) {
    assert!(matches!(error, HookError::Failed(_)), "{error:?}");
    let text = error.to_string();
    assert!(text.contains("got no reply"), "{text}");
    assert!(text.contains("1 dropped, scope this_process"), "{text}");
    assert!(text.contains("not recorded"), "{text}");
    assert!(text.contains(reason), "{text}");
    assert_eq!(drops.snapshot().dropped, 1);
}

#[test]
fn a_missing_directory_is_not_created_and_the_miss_is_still_the_same_error() {
    let dir = tempfile::tempdir().unwrap();
    let absent = dir.path().join("not-there");
    let socket = socket_in(&absent);
    let drops = recording();
    let error = run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply");
    assert_unrecorded(&error, &drops, "directory does not exist");
    assert!(!absent.exists(), "the hook created a directory");
}

#[test]
fn a_symlinked_record_is_not_written_through() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, b"").unwrap();
    std::os::unix::fs::symlink(&target, record_path(&socket)).unwrap();
    let drops = recording();
    let error = run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply");
    assert_unrecorded(&error, &drops, "symbolic link");
    assert_eq!(std::fs::metadata(&target).unwrap().len(), 0);
}

#[test]
fn a_record_at_the_cap_is_not_appended_to() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let full = vec![b'x'; HOOK_STOP_BYTES as usize];
    std::fs::write(record_path(&socket), &full).unwrap();
    let drops = recording();
    let error = run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply");
    assert_unrecorded(&error, &drops, "64 KiB cap");
    assert_eq!(
        std::fs::metadata(record_path(&socket)).unwrap().len(),
        HOOK_STOP_BYTES
    );
}

#[test]
fn a_record_one_byte_under_the_cap_still_takes_a_line() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    std::fs::write(
        record_path(&socket),
        vec![b'x'; HOOK_STOP_BYTES as usize - 1],
    )
    .unwrap();
    let drops = recording();
    run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .unwrap_err();
    assert!(std::fs::metadata(record_path(&socket)).unwrap().len() > HOOK_STOP_BYTES);
}

/// A FIFO at the record's name would hold `open` until something read it. The
/// hook is inside its budget: it must come back at once, and say it is not a
/// record it can write.
#[test]
fn a_fifo_at_the_records_name_does_not_hold_the_hook() {
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let fifo = std::ffi::CString::new(record_path(&socket).as_os_str().as_bytes()).unwrap();
    // SAFETY: `fifo` is a NUL-terminated path that outlives the call.
    assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);
    let drops = recording();
    let started = std::time::Instant::now();
    let error = run("session_ended", &socket, &drops, |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    assert!(error.to_string().contains("not recorded"), "{error}");
    assert_eq!(drops.snapshot().dropped, 1);
}

#[test]
fn the_record_is_private_and_a_session_key_that_is_not_one_is_left_out() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let record = DropRecord::with_clock(fixed_clock);
    record
        .append(
            &socket,
            "session_ended",
            Some("a\"b\n{\"event\":\"forged\"}"),
        )
        .unwrap();
    record
        .append(&socket, "idle_prompt", Some(&"k".repeat(65)))
        .unwrap();
    record.append(&socket, "idle_prompt", None).unwrap();
    let mode = std::fs::metadata(record_path(&socket))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
    let lines = record_lines(&socket);
    assert_eq!(lines.len(), 3);
    for line in &lines {
        assert!(line.get("session").is_none(), "{line}");
    }
    assert_eq!(lines[0]["event"], json!("session_ended"));
}

/// N hooks missing at once is the case the record exists for: a stalled app
/// makes every hook on the machine miss together. One `write()` of one short
/// line to an `O_APPEND` file keeps each whole.
#[test]
fn concurrent_appends_yield_that_many_whole_lines() {
    const THREADS: usize = 16;
    const PER_THREAD: usize = 25;
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(THREADS));
    let workers: Vec<_> = (0..THREADS)
        .map(|worker| {
            let socket = socket.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                let record = DropRecord::with_clock(fixed_clock);
                let session = format!("term-{worker}-1");
                barrier.wait();
                for _ in 0..PER_THREAD {
                    record
                        .append(&socket, "elicitation_url_dialog", Some(&session))
                        .expect("appended");
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().unwrap();
    }
    let lines = record_lines(&socket);
    assert_eq!(lines.len(), THREADS * PER_THREAD);
    for line in lines {
        assert_eq!(line["event"], json!("elicitation_url_dialog"), "{line}");
        assert!(line["session"].as_str().unwrap().starts_with("term-"));
    }
}

/// The process's own count is the one that writes; this is the production
/// route (`run_notify`, resolving the socket from the environment) against a
/// socket in a directory the test owns.
#[test]
fn the_process_count_writes_the_record_beside_the_socket_the_environment_names() {
    let serial = crate::test_support::env::env_serial();
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, "term-4-1")
        .set(crate::alerts::bridge::SOCKET_ENV, &socket);
    let error = run_notify(Some("session_ended"), &input(), |_, _| {
        Err(SendError::NoReply)
    })
    .expect_err("no reply");
    assert!(!error.to_string().contains("not recorded"), "{error}");
    let lines = record_lines(&socket);
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["event"], json!("session_ended"));
    assert!(lines[0]["at"].as_u64().is_some_and(|at| at > FIXED_AT));
}
