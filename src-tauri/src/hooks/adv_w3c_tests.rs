//! W3c adversary pass on the hook's half of the durable record of notify
//! replies it stopped waiting for (ft-6d8ff969, item 4).
//!
//! Each test names the attack. A test here passes on the lane tip unless its
//! name says it demonstrates a break (those are written to FAIL on the tip and
//! none is kept that does not).

use super::notify_drops::{record_path, DropRecord, NotifyDrops};
use super::*;
use crate::procguard::LockedSpawn;
use std::os::unix::fs::PermissionsExt;
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

fn miss(
    socket: &Path,
    drops: &NotifyDrops,
    event: &str,
    session: &str,
) -> Result<HookOutput, HookError> {
    let serial = crate::test_support::env::env_serial();
    let _env = crate::test_support::env::bind_env(&serial)
        .set(crate::alerts::bridge::SESSION_ENV, session);
    let socket: PathBuf = socket.to_path_buf();
    run_notify_with(
        Some(event),
        &input(),
        |_, _| Err(SendError::NoReply),
        drops,
        Some(Duration::from_secs(30)),
        true,
        move || Some(socket),
    )
}

/// The Err text a hook with no sink at all gives for the same miss, which the
/// record must leave byte for byte alone up to the one clause it may add.
fn unrecorded_text(event: &str) -> String {
    let dir = tempfile::tempdir().unwrap();
    miss(
        &dir.path().join("hook.sock"),
        &NotifyDrops::new(),
        event,
        "term-4-1",
    )
    .expect_err("no reply")
    .to_string()
}

/// A dangling symlink at the record's name is the classic way to make an
/// `O_CREAT` open create a file somewhere the attacker chose. `O_NOFOLLOW`
/// must refuse it without creating the target.
#[test]
fn adv_w3c_a_dangling_symlink_at_the_record_is_neither_followed_nor_created_through() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    let target = dir.path().join("planted-target");
    std::os::unix::fs::symlink(&target, record_path(&socket)).unwrap();
    let drops = recording();
    let error = miss(&socket, &drops, "session_ended", "term-4-1").expect_err("no reply");
    assert!(error.to_string().contains("not recorded"), "{error}");
    assert!(
        !target.exists(),
        "the hook created a file through a symlink"
    );
    assert_eq!(drops.snapshot().dropped, 1);
}

/// A directory squatting on the record's name: the open must fail at once, the
/// clause must say so, and the Err and the count must not change.
#[test]
fn adv_w3c_a_directory_at_the_records_name_keeps_the_error_and_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    std::fs::create_dir(record_path(&socket)).unwrap();
    // Timed on the append itself: `miss` first waits for the env lock, which
    // other tests hold for as long as they like.
    let started = std::time::Instant::now();
    let reason = DropRecord::with_clock(fixed_clock)
        .append(&socket, "session_ended", None)
        .expect_err("a directory is not a record");
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "{reason}"
    );
    let drops = recording();
    let error = miss(&socket, &drops, "session_ended", "term-4-1").expect_err("no reply");
    let text = error.to_string();
    assert!(text.contains("got no reply"), "{text}");
    assert!(text.contains("not recorded"), "{text}");
    assert!(text.contains("1 dropped, scope this_process"), "{text}");
    assert_eq!(drops.snapshot().dropped, 1);
}

/// A directory the hook may not write to (EACCES, not ENOENT): same contract.
#[test]
fn adv_w3c_a_read_only_directory_keeps_the_error_and_the_count() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o500)).unwrap();
    let probe = std::fs::File::create(dir.path().join("probe"));
    if probe.is_ok() {
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        return; // a superuser writes anyway
    }
    let drops = recording();
    let error = miss(&socket, &drops, "session_ended", "term-4-1");
    std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
    let text = error.expect_err("no reply").to_string();
    assert!(text.contains("not recorded"), "{text}");
    assert_eq!(drops.snapshot().dropped, 1);
}

/// The only difference the record may make to the Err is one clause inside the
/// existing parenthesis. Everything before it, and the closing text, is the
/// text a sink-less count gives.
#[test]
fn adv_w3c_the_err_text_differs_from_the_sinkless_text_only_by_the_clause() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("gone").join("hook.sock");
    let with = miss(&socket, &recording(), "session_ended", "term-4-1")
        .expect_err("no reply")
        .to_string();
    let without = unrecorded_text("session_ended");
    let (head, tail) = without.split_at(without.rfind(')').expect("closing paren"));
    assert!(with.starts_with(head), "{with} vs {without}");
    assert!(with.ends_with(tail), "{with} vs {without}");
    let clause = &with[head.len()..with.len() - tail.len()];
    assert!(clause.starts_with("; not recorded: "), "{clause}");
    assert!(
        !clause.contains('\n'),
        "the clause broke the one-line shape"
    );
}

/// Every event the bridge speaks, with a session key at the recorded maximum:
/// each line is whole, under 200 bytes and parses back.
#[test]
fn adv_w3c_every_event_with_a_maximal_session_key_fits_one_short_line() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    let record = DropRecord::with_clock(|| Some(u64::MAX));
    let key = "k".repeat(64);
    for event in crate::alerts::bridge::EVENTS.iter() {
        record
            .append(&socket, event.name, Some(&key))
            .unwrap_or_else(|reason| panic!("{}: {reason}", event.name));
    }
    let text = std::fs::read_to_string(record_path(&socket)).unwrap();
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    assert_eq!(lines.len(), crate::alerts::bridge::EVENTS.len());
    for raw in lines {
        assert!(raw.ends_with('\n'));
        assert!(raw.len() < 200, "{} bytes: {raw}", raw.len());
        let parsed: Value = serde_json::from_str(raw).expect("one parsable object");
        assert_eq!(parsed["session"], json!(key));
    }
}

/// Characterizes (does not judge) what the ruling's "mode 0600" means for a
/// record that already exists with a wider mode: the mode argument of `open`
/// applies at creation only, so the hook appends to it and leaves it alone.
#[test]
fn adv_w3c_an_existing_wide_record_keeps_its_mode_and_still_takes_the_line() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    std::fs::write(record_path(&socket), b"").unwrap();
    std::fs::set_permissions(record_path(&socket), std::fs::Permissions::from_mode(0o644)).unwrap();
    DropRecord::with_clock(fixed_clock)
        .append(&socket, "session_ended", None)
        .expect("appended");
    let mode = std::fs::metadata(record_path(&socket))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o644, "the hook neither widened nor narrowed it");
}

/// A record is created 0600 whatever the umask says (a umask can only remove
/// bits, so even a 0 umask gives at most the requested 0600).
#[test]
fn adv_w3c_a_new_record_is_0600_under_a_zero_umask() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    // SAFETY: umask is process state; the serial lock keeps other env-touching
    // tests out while it is changed, and it is put back before the lock drops.
    let serial = crate::test_support::env::env_serial();
    let before = unsafe { libc::umask(0) };
    let appended = DropRecord::with_clock(fixed_clock).append(&socket, "session_ended", None);
    unsafe { libc::umask(before) };
    drop(serial);
    appended.expect("appended");
    let mode = std::fs::metadata(record_path(&socket))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o600, "{mode:o}");
}

const CHILD_SOCKET: &str = "ADV_W3C_CHILD_SOCKET";
const CHILD_LINES: usize = 40;

/// Not a test of its own: the body each child process of the cross-process
/// test below runs. With the variable unset (every ordinary run) it does
/// nothing.
#[test]
fn adv_w3c_child_appender() {
    let Some(socket) = std::env::var_os(CHILD_SOCKET) else {
        return;
    };
    let socket = PathBuf::from(socket);
    let record = DropRecord::with_clock(fixed_clock);
    let session = format!("term-{}-1", std::process::id());
    for _ in 0..CHILD_LINES {
        record
            .append(&socket, "session_ended", Some(&session))
            .expect("appended");
    }
}

/// Processes, not threads: the hook is one process per event, so the append
/// has to be whole across address spaces. The children are this test binary
/// re-run on one test, with no socket or descriptor to keep from them; the
/// spawn still goes through `LockedSpawn`, so another test's pipe is never
/// held open by one of these children.
#[test]
fn adv_w3c_separate_processes_appending_yield_that_many_whole_lines() {
    const PROCESSES: usize = 8;
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("hook.sock");
    let exe = std::env::current_exe().expect("test binary");
    let children: Vec<_> = (0..PROCESSES)
        .map(|_| {
            std::process::Command::new(&exe)
                .args([
                    "--exact",
                    "hooks::adv_w3c_tests::adv_w3c_child_appender",
                    "--test-threads=1",
                ])
                .env(CHILD_SOCKET, &socket)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn_locked()
                .expect("spawn child")
        })
        .collect();
    for child in children {
        let out = child.wait_with_output().expect("child");
        assert!(
            out.status.success(),
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let text = std::fs::read_to_string(record_path(&socket)).unwrap();
    let mut sessions = std::collections::BTreeSet::new();
    let mut count = 0;
    for raw in text.lines() {
        let line: Value = serde_json::from_str(raw).unwrap_or_else(|e| panic!("torn {raw:?}: {e}"));
        assert_eq!(line["event"], json!("session_ended"));
        sessions.insert(line["session"].as_str().unwrap().to_owned());
        count += 1;
    }
    assert_eq!(count, PROCESSES * CHILD_LINES);
    assert_eq!(sessions.len(), PROCESSES);
}
