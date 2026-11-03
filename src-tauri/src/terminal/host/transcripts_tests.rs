use super::*;
use std::os::unix::fs::symlink;
use tempfile::TempDir;

/// A temporary directory with no symlink in its path, as the host's own
/// directory has: `private_dir` refuses one (macOS's `/var` is a link).
fn private_tempdir() -> TempDir {
    tempfile::tempdir_in(std::env::temp_dir().canonicalize().unwrap()).unwrap()
}

fn small() -> Limits {
    Limits {
        segment_bytes: 256,
        total_bytes: 64 * 1024 * 1024,
        queue_bytes: 1024 * 1024,
        max_age: Duration::from_secs(14 * 24 * 60 * 60),
    }
}

fn store(dir: &TempDir, limits: Limits) -> Transcripts {
    Transcripts::open(dir.path().join("transcripts"), limits).unwrap()
}

fn files(dir: &TempDir) -> PathBuf {
    dir.path().join("transcripts")
}

fn tail(read: TranscriptRead) -> (Vec<u8>, TranscriptTail) {
    match read {
        TranscriptRead::Found(tail) => (BASE64.decode(&tail.data_b64).unwrap(), tail),
        other => panic!("expected a transcript, got {other:?}"),
    }
}

/// Bytes whose value says where in the stream they are, so a read that
/// skipped, repeated or reordered some is caught by comparison.
fn stream(len: usize) -> Vec<u8> {
    (0..len).map(|i| b'a' + (i % 26) as u8).collect()
}

fn record(store: &Transcripts, session: &str, bytes: &[u8], chunk: usize) {
    let tap = store.arm(session, None).unwrap();
    for piece in bytes.chunks(chunk) {
        tap.chunk(piece);
    }
}

fn age(path: &Path, by: Duration) {
    let file = File::options().write(true).open(path).unwrap();
    file.set_modified(SystemTime::now() - by).unwrap();
}

fn names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn output_is_appended_in_order_and_the_files_are_the_users_alone() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let expected = stream(200);
    let running = store.arm("term-1-1", None).unwrap();
    for piece in expected.chunks(37) {
        running.chunk(piece);
    }
    let (got, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert_eq!(got, expected);
    assert!(!tail.truncated);
    assert!(tail.settled);
    assert!(tail.live, "the session's ring still holds its tap");
    drop(running);
    assert_eq!(tail.retained, 200);
    assert_eq!(tail.gap_markers, 0);
    let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(&files(&dir)), 0o700);
    assert_eq!(mode(&files(&dir).join("term-1-1.log")), 0o600);
}

#[test]
fn a_finished_session_is_no_longer_live_and_keeps_its_output() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(b"hello\r\n");
    drop(tap);
    let (got, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert_eq!(got, b"hello\r\n");
    assert!(!tail.live);
}

#[test]
fn a_full_segment_becomes_the_previous_one_and_nothing_is_lost_until_the_second_rotation() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let expected = stream(400);
    record(&store, "term-1-1", &expected, 90);
    let (got, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert_eq!(
        got, expected,
        "both segments, joined without the continuation line"
    );
    assert!(!tail.truncated, "the session's first byte is still on disk");
    assert_eq!(names(&files(&dir)), ["term-1-1.log", "term-1-1.log.1"]);
}

#[test]
fn rotation_bounds_a_session_to_two_segments_and_says_the_start_is_gone() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let expected = stream(5000);
    record(&store, "term-1-1", &expected, 100);
    let (got, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert!(!got.is_empty());
    assert!(
        expected.ends_with(&got),
        "what is left is the newest output, in order"
    );
    assert!(tail.truncated, "earlier output was rotated away");
    assert_eq!(names(&files(&dir)).len(), 2);
    for name in names(&files(&dir)) {
        let len = std::fs::metadata(files(&dir).join(name)).unwrap().len();
        assert!(len <= 256, "a segment is at most the segment size: {len}");
    }
    assert!(tail.retained <= 2 * 256);
}

#[test]
fn a_read_is_clipped_to_the_newest_bytes_it_was_asked_for() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let expected = stream(200);
    record(&store, "term-1-1", &expected, 50);
    let (got, tail) = tail(store.read("term-1-1", 64, false, None));
    assert_eq!(got, &expected[136..]);
    assert!(tail.truncated, "clipped by the bound");
    assert_eq!(tail.retained, 200);
    assert_eq!(tail.bytes, 64);
}

#[test]
fn a_session_with_no_transcript_is_missing_and_a_bad_name_is_not() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    assert_eq!(
        store.read("term-9-9", 100, false, None),
        TranscriptRead::Missing
    );
    for bad in ["../x", "term-1-a/../../y", "", "term-x-1"] {
        match store.read(bad, 100, false, None) {
            TranscriptRead::Unreadable { reason } => {
                assert!(reason.contains("not a session id"), "{reason}")
            }
            other => panic!("{bad:?} read as {other:?}"),
        }
    }
}

#[test]
fn a_transcript_that_is_a_link_is_unreadable_and_never_followed() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let outside = dir.path().join("outside");
    std::fs::write(&outside, "not a transcript").unwrap();
    symlink(&outside, files(&dir).join("term-1-1.log")).unwrap();
    match store.read("term-1-1", 100, false, None) {
        TranscriptRead::Unreadable { reason } => {
            assert!(reason.contains("symbolic link"), "{reason}");
            assert!(!reason.contains("not a transcript"));
        }
        other => panic!("a link read as {other:?}"),
    }
    symlink(&outside, files(&dir).join("term-1-2.log.1")).unwrap();
    assert!(matches!(
        store.read("term-1-2", 100, false, None),
        TranscriptRead::Unreadable { .. }
    ));
}

#[test]
fn a_link_in_the_way_of_a_new_session_is_replaced_not_written_through() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let outside = dir.path().join("outside");
    std::fs::write(&outside, "untouched").unwrap();
    symlink(&outside, files(&dir).join("term-1-1.log")).unwrap();
    record(&store, "term-1-1", b"fresh", 5);
    let (got, _) = tail(store.read("term-1-1", 100, false, None));
    assert_eq!(got, b"fresh");
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
    assert!(std::fs::symlink_metadata(files(&dir).join("term-1-1.log"))
        .unwrap()
        .is_file());
    // And the open itself refuses a link, whatever put it there.
    let planted = files(&dir).join("term-1-3.log");
    symlink(&outside, &planted).unwrap();
    assert!(Inner::create_file(&planted).is_err());
    assert_eq!(std::fs::read(&outside).unwrap(), b"untouched");
}

#[test]
fn a_file_with_wider_permissions_is_narrowed_before_anything_is_written() {
    let dir = private_tempdir();
    let target = dir.path().join("loose.log");
    std::fs::write(&target, b"").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o644)).unwrap();
    Inner::create_file(&target).unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

/// The bytes of `text` that are output, and the bytes its gap markers say were
/// dropped.
fn accounted(text: &str) -> (u64, u64) {
    let mut recorded = 0u64;
    let mut dropped = 0u64;
    let mut rest = text;
    while let Some(at) = rest.find("\r\n[gitpulse: ") {
        recorded += rest[..at].len() as u64;
        let after = &rest[at + "\r\n[gitpulse: ".len()..];
        let (n, tail_text) = after.split_once(' ').unwrap();
        dropped += n.parse::<u64>().unwrap();
        rest = tail_text
            .strip_prefix("bytes of output were not recorded]\r\n")
            .unwrap();
    }
    recorded += rest.len() as u64;
    (recorded, dropped)
}

#[test]
fn output_that_outruns_the_writer_is_dropped_with_a_marker_and_never_stalls_the_reader() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            queue_bytes: 2000,
            // Room for all 60000 bytes: nothing here may rotate away.
            segment_bytes: 1024 * 1024,
            ..small()
        },
    );
    // The writer is held up on its own lock; the reader side must not be.
    let stalled = lock(&store.inner.files);
    let tap = store.arm("term-1-1", None).unwrap();
    let started = std::time::Instant::now();
    let chunk = vec![b'x'; 300];
    for _ in 0..200 {
        tap.chunk(&chunk);
    }
    assert!(
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(2)),
        "pushing 60000 bytes at a stalled writer took {:?}",
        started.elapsed()
    );
    drop(tap);
    drop(stalled);
    let (got, tail) = tail(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert!(tail.gap_markers >= 1, "{} markers", tail.gap_markers);
    let text = String::from_utf8(got).unwrap();
    let (recorded, dropped) = accounted(&text);
    assert_eq!(
        recorded + dropped,
        200 * 300,
        "every byte is either in the log or counted in a marker"
    );
    assert!(dropped > 0 && recorded > 0);
}

/// An answer as it crosses the socket, so a test can name a field the way the
/// renderer reads it.
fn wire(read: TranscriptRead) -> serde_json::Value {
    serde_json::to_value(read).unwrap()
}

#[test]
fn a_read_before_the_session_ends_reports_the_bytes_dropped_and_not_yet_marked() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            queue_bytes: 2000,
            segment_bytes: 1024 * 1024,
            ..small()
        },
    );
    let tap = store.arm("term-1-1", None).unwrap();
    assert!(store.flush(Duration::from_secs(5)), "the session opens");
    // Stalled only now, with the file open, so a read finds a log to read.
    let stalled = lock(&store.inner.files);
    let chunk = vec![b'x'; 300];
    for _ in 0..200 {
        tap.chunk(&chunk);
    }
    // How many chunks the writer took before it stalled is the scheduler's,
    // and a chunk that fits after a drop queues the marker for the drops before
    // it. What overflowed since the last such marker has none queued and the
    // session is still open: the log shows none, and only the read can say
    // they are missing.
    let during = wire(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert_eq!(during["state"], "found", "{during}");
    assert_eq!(during["live"], true);
    assert_eq!(during["settled"], false, "the writer is stalled");
    assert_eq!(during["gap_markers"], 0);
    let pending = during["unrecorded_bytes"].as_u64().unwrap_or(0);
    assert!(
        (1..=199).contains(&(pending / 300)) && pending.is_multiple_of(300),
        "a live read hid the bytes already dropped: {during}"
    );
    drop(tap);
    drop(stalled);
    assert!(store.flush(Duration::from_secs(5)));
    let after = wire(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert_eq!(
        after["unrecorded_bytes"], 0,
        "the session's end queued the marker"
    );
    // The live log had no marker yet, so what the live read counted is what the
    // markers then carried between them, and with it every byte is accounted for.
    let text =
        String::from_utf8(BASE64.decode(after["data_b64"].as_str().unwrap()).unwrap()).unwrap();
    let (recorded, dropped) = accounted(&text);
    assert_eq!(dropped, pending, "the live read said {pending}");
    assert_eq!(recorded + dropped, 200 * 300);
}

#[test]
fn a_read_counts_the_drops_whose_marker_is_queued_or_taken_but_not_yet_in_the_file() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            queue_bytes: 2000,
            segment_bytes: 1024 * 1024,
            ..small()
        },
    );
    let tap = store.arm("term-1-1", None).unwrap();
    assert!(store.flush(Duration::from_secs(5)), "the session opens");
    let chunk = vec![b'x'; 300];
    let mut printed = 0u64;
    // Drops with no marker queued for them: the writer is held, so nothing
    // that overflowed was freed for a later chunk to follow.
    let stalled = lock(&store.inner.files);
    for _ in 0..200 {
        tap.chunk(&chunk);
        printed += 300;
    }
    drop(stalled);
    assert!(store.flush(Duration::from_secs(5)), "the writer drains");
    // The writer is now stopped at the marker the next chunk queues for them:
    // the marker is out of the queue's counts and not yet in the file.
    let hold = lock(&store.inner.gap_hold);
    let first = lock(&store.inner.queue)
        .gaps
        .get("term-1-1")
        .copied()
        .unwrap_or(0);
    assert!(first > 0, "the overflow left bytes without a marker");
    tap.chunk(&chunk);
    printed += 300;
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while !lock(&store.inner.queue).ops.is_empty() {
        assert!(
            std::time::Instant::now() < deadline,
            "the writer never took the marker's batch"
        );
        std::thread::yield_now();
    }
    // More overflow behind the stopped writer.
    for _ in 0..20 {
        tap.chunk(&chunk);
        printed += 300;
    }
    let second = lock(&store.inner.queue)
        .gaps
        .get("term-1-1")
        .copied()
        .unwrap_or(0);
    assert!(second > 0, "the second overflow left bytes without a marker");
    let during = wire(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert_eq!(during["state"], "found", "{during}");
    assert_eq!(during["settled"], false, "the writer is stopped");
    assert_eq!(
        during["unrecorded_bytes"].as_u64(),
        Some(first + second),
        "a live read hid dropped bytes: {first} wait on a marker the writer holds and is \
         not in the file, {second} have none queued: {during}"
    );
    drop(hold);
    drop(tap);
    assert!(store.flush(Duration::from_secs(5)));
    let after = wire(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert_eq!(after["unrecorded_bytes"], 0, "{after}");
    let text =
        String::from_utf8(BASE64.decode(after["data_b64"].as_str().unwrap()).unwrap()).unwrap();
    let (recorded, dropped) = accounted(&text);
    assert_eq!(recorded + dropped, printed, "every byte is accounted for");
}

#[test]
fn output_after_the_store_began_stopping_is_counted_as_unrecorded_even_once_the_session_ends() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(b"before");
    store.shutdown();
    let late = b"after the host stopped";
    tap.chunk(late);
    drop(tap);
    let answer = wire(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false, None));
    assert_eq!(answer["state"], "found", "{answer}");
    assert_eq!(
        answer["unrecorded_bytes"],
        late.len(),
        "bytes dropped after shutdown began were forgotten: {answer}"
    );
    assert_eq!(answer["live"], false);
}

#[test]
fn a_directory_over_its_total_drops_the_oldest_finished_session_and_never_a_running_one() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            // A finished session is its 200 bytes and the 31 of its end marker.
            total_bytes: 700,
            ..small()
        },
    );
    let running = store.arm("term-1-9", None).unwrap();
    running.chunk(&stream(200));
    for session in ["term-1-1", "term-1-2"] {
        record(&store, session, &stream(200), 200);
    }
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(names(&files(&dir)).len(), 3, "662 bytes fit in 700");
    // Oldest first, and the running session is the very oldest of all.
    age(
        &files(&dir).join("term-1-9.log"),
        Duration::from_secs(4 * 3600),
    );
    age(
        &files(&dir).join("term-1-1.log"),
        Duration::from_secs(3 * 3600),
    );
    age(
        &files(&dir).join("term-1-2.log"),
        Duration::from_secs(2 * 3600),
    );
    record(&store, "term-1-3", &stream(200), 200);
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(
        names(&files(&dir)),
        ["term-1-2.log", "term-1-3.log", "term-1-9.log"],
        "the oldest finished session went, though a running one is older"
    );
    record(&store, "term-1-4", &stream(200), 200);
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(
        names(&files(&dir)),
        ["term-1-3.log", "term-1-4.log", "term-1-9.log"]
    );
    drop(running);
}

#[test]
fn transcripts_past_their_age_are_deleted_when_the_host_starts() {
    let dir = private_tempdir();
    {
        let store = store(&dir, small());
        record(&store, "term-1-1", b"old", 3);
        record(&store, "term-1-2", b"recent", 6);
    }
    age(
        &files(&dir).join("term-1-1.log"),
        Duration::from_secs(15 * 24 * 3600),
    );
    let store = store(&dir, small());
    assert_eq!(
        store.read("term-1-1", 100, false, None),
        TranscriptRead::Missing
    );
    assert_eq!(tail(store.read("term-1-2", 100, false, None)).0, b"recent");
}

#[test]
fn a_transcript_survives_the_host_that_wrote_it() {
    let dir = private_tempdir();
    let expected = stream(1000);
    {
        let store = store(&dir, small());
        record(&store, "term-1-1", &expected, 64);
        // The host goes: queued output is written out, the writer stops.
    }
    let restarted = store(&dir, small());
    let (got, tail) = tail(restarted.read("term-1-1", 4096, false, None));
    assert!(expected.ends_with(&got));
    assert!(got.len() >= 256 - CONTINUED.len());
    assert!(!tail.live, "no running session belongs to the new host");
}

#[test]
fn a_new_session_under_an_old_id_does_not_inherit_its_output() {
    let dir = private_tempdir();
    {
        let store = store(&dir, small());
        record(&store, "term-1-1", b"the earlier session", 19);
    }
    let store = store(&dir, small());
    record(&store, "term-1-1", b"this one", 8);
    assert_eq!(
        tail(store.read("term-1-1", 100, false, None)).0,
        b"this one"
    );
}

#[test]
fn a_session_that_never_started_leaves_nothing_but_a_running_one_is_never_discarded() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(b"live");
    store.discard("term-1-1");
    assert_eq!(tail(store.read("term-1-1", 100, false, None)).0, b"live");
    drop(tap);
    store.discard("term-1-1");
    assert_eq!(
        store.read("term-1-1", 100, false, None),
        TranscriptRead::Missing
    );
}

#[test]
fn a_session_is_recorded_once_at_a_time() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let first = store.arm("term-1-1", None).unwrap();
    assert!(matches!(
        store.arm("term-1-1", None),
        Err(ArmError::Duplicate)
    ));
    drop(first);
    assert!(store.arm("term-1-1", None).is_ok());
}

#[test]
fn a_write_that_cannot_happen_is_reported_not_called_missing() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    std::fs::remove_dir_all(files(&dir)).unwrap();
    record(&store, "term-1-1", b"lost", 4);
    match store.read("term-1-1", 100, false, None) {
        TranscriptRead::Unreadable { reason } => {
            assert!(reason.contains("could not be recorded"), "{reason}")
        }
        other => panic!("a failed recording read as {other:?}"),
    }
}

#[test]
fn a_read_can_mask_secrets_before_they_leave_the_host() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let secret = "ghp_0123456789abcdefghijklmnopqrstuvwxyzA";
    record(
        &store,
        "term-1-1",
        format!("pushing with {secret} now\r\n").as_bytes(),
        64,
    );
    let (raw, plain) = tail(store.read("term-1-1", 4096, false, None));
    assert!(String::from_utf8(raw).unwrap().contains(secret));
    assert!(!plain.redacted);
    let (masked, tail) = tail(store.read("term-1-1", 4096, true, None));
    let masked = String::from_utf8(masked).unwrap();
    assert!(!masked.contains(secret), "{masked}");
    assert!(masked.contains("pushing with"));
    assert!(tail.redacted);
}

#[test]
fn a_stopped_store_takes_no_new_sessions() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    store.shutdown();
    assert!(matches!(store.arm("term-1-1", None), Err(ArmError::Closed)));
}

// -- how a recording ended, kept on disk --------------------------------------

/// A host that died, or was killed, before it closed a session's transcript
/// leaves a file that reads like any other. It must not read as complete.
#[test]
fn a_transcript_the_host_never_closed_reads_as_possibly_incomplete() {
    let dir = private_tempdir();
    {
        let store = store(&dir, small());
        let tap = store.arm("term-1-1", None).unwrap();
        tap.chunk(b"started\r\n");
        assert!(store.flush(Duration::from_secs(5)));
        // The host goes before the session's close is ever applied.
        store.shutdown();
        drop(tap);
    }
    let restarted = store(&dir, small());
    let (got, tail) = tail(restarted.read("term-1-1", 4096, false, None));
    assert_eq!(got, b"started\r\n");
    assert!(!tail.live, "no running session belongs to the new host");
    let error = tail
        .recording_error
        .expect("an end nobody wrote down is not a complete transcript");
    assert!(error.contains("did not close"), "{error}");
}

/// Why a recording failed is on disk beside it, so a later host can still say.
#[test]
fn a_recording_failure_is_remembered_by_the_host_that_reads_it_later() {
    let dir = private_tempdir();
    {
        let store = store(&dir, small());
        let tap = store.arm("term-1-1", None).unwrap();
        tap.chunk(&stream(100));
        assert!(store.flush(Duration::from_secs(5)));
        // The next rotation cannot happen: its target is a directory.
        let previous = files(&dir).join("term-1-1.log.1");
        std::fs::create_dir(&previous).unwrap();
        tap.chunk(&stream(300));
        assert!(store.flush(Duration::from_secs(5)));
        std::fs::remove_dir(&previous).unwrap();
        let (_, now) = tail(store.read("term-1-1", 4096, false, None));
        assert!(now.recording_error.is_some(), "the failing host knows");
        drop(tap);
    }
    let restarted = store(&dir, small());
    let (got, tail) = tail(restarted.read("term-1-1", 4096, false, None));
    assert_eq!(got.len(), 256, "what was written before the failure");
    let error = tail
        .recording_error
        .expect("the reason survived the host that saw it");
    assert!(error.contains("term-1-1.log"), "{error}");
    assert!(!error.contains("did not close"), "{error}");
}

/// The ordinary case, which the end marker must not disturb: a session that
/// was closed reads clean after a restart, and the marker is not text.
#[test]
fn a_session_closed_normally_reads_clean_after_a_restart_and_the_end_marker_is_not_text() {
    let dir = private_tempdir();
    {
        let store = store(&dir, small());
        record(&store, "term-1-1", b"all of it\r\n", 4);
        assert!(store.flush(Duration::from_secs(5)));
        let raw = std::fs::read(files(&dir).join("term-1-1.log")).unwrap();
        assert!(
            String::from_utf8_lossy(&raw).contains("transcript ends]"),
            "the file records that it was closed: {raw:?}"
        );
    }
    let restarted = store(&dir, small());
    let (got, tail) = tail(restarted.read("term-1-1", 4096, false, None));
    assert_eq!(got, b"all of it\r\n");
    assert_eq!(tail.recording_error, None);
    assert_eq!(tail.retained, 11, "the marker is not output");
    assert!(!tail.truncated);
}

/// The marker is written under the same size bound as the output.
#[test]
fn the_end_marker_never_pushes_a_segment_past_its_size() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    // 250 of 256 bytes: too little room left for the marker.
    let expected = stream(250);
    record(&store, "term-1-1", &expected, 50);
    assert!(store.flush(Duration::from_secs(5)));
    for name in names(&files(&dir)) {
        let len = std::fs::metadata(files(&dir).join(&name)).unwrap().len();
        assert!(len <= 256, "{name} is {len} bytes");
    }
    let (got, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert_eq!(got, expected);
    assert_eq!(tail.recording_error, None);
}

// -- hazards on the way in ----------------------------------------------------

fn within<T: Send + 'static>(what: &str, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (send, receive) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = send.send(f());
    });
    receive
        .recv_timeout(Duration::from_secs(5))
        .unwrap_or_else(|_| panic!("{what} did not return: it is blocked on a file"))
}

fn fifo(path: &Path) {
    let name = std::ffi::CString::new(path.to_str().unwrap()).unwrap();
    // SAFETY: `name` is a valid NUL-terminated path for the length of the call.
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
}

/// Anything the user can plant in a `0700` directory: a named pipe in place of
/// a segment must be reported, not waited on.
#[test]
fn a_named_pipe_in_place_of_a_transcript_is_unreadable_and_blocks_nothing() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    fifo(&files(&dir).join("term-1-1.log"));
    match within("reading a pipe", move || {
        store.read("term-1-1", 100, false, None)
    }) {
        TranscriptRead::Unreadable { reason } => {
            assert!(reason.contains("not a file"), "{reason}")
        }
        other => panic!("a pipe read as {other:?}"),
    }
}

#[test]
fn a_named_pipe_in_the_way_of_a_new_segment_is_refused_and_blocks_nothing() {
    let dir = private_tempdir();
    let planted = files(&dir);
    std::fs::create_dir_all(&planted).unwrap();
    let planted = planted.join("term-1-3.log");
    fifo(&planted);
    assert!(within("creating over a pipe", move || Inner::create_file(&planted)).is_err());
}

// -- the end of the session's output, after shutdown --------------------------

#[test]
fn a_store_that_stopped_settles_even_when_a_session_ends_afterwards() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(b"late");
    store.shutdown();
    tap.chunk(b"after the host stopped");
    drop(tap);
    assert!(
        store.flush(Duration::from_millis(300)),
        "nothing queued after the stop can ever be written, so nothing waits for it"
    );
}

// -- redaction needs the context before the window ----------------------------

#[test]
fn a_value_whose_name_is_before_the_window_is_still_masked() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            segment_bytes: 1024 * 1024,
            ..small()
        },
    );
    let value = "Zq7Lw9Xv2Rk8Tn5Jm3Hs6Bd1Fg0CaPe4Yu";
    record(
        &store,
        "term-1-1",
        format!("connecting\r\npassword={value}\r\ndone\r\n").as_bytes(),
        64,
    );
    // The window starts inside the value, after the name that marks it secret.
    let (masked, tail) = tail(store.read("term-1-1", 24, true, None));
    let masked = String::from_utf8(masked).unwrap();
    assert!(!masked.contains(&value[value.len() - 12..]), "{masked:?}");
    assert!(tail.redacted);
    assert!(tail.truncated);
}

#[test]
fn the_tail_of_a_private_key_whose_first_line_is_before_the_window_is_masked() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            segment_bytes: 8 * 1024 * 1024,
            ..small()
        },
    );
    let body: String = (0..2000)
        .map(|i| format!("QUJDREVGR0hJSktMTU5PUFFSU1RVVldYWVo{i:04}\r\n"))
        .collect();
    record(
        &store,
        "term-1-1",
        format!("-----BEGIN PRIVATE KEY-----\r\n{body}-----END PRIVATE KEY-----\r\nafter\r\n")
            .as_bytes(),
        4096,
    );
    // Both the cut and the lookback land inside the key's body.
    let (masked, tail) = tail(store.read("term-1-1", 1000, true, None));
    let masked = String::from_utf8(masked).unwrap();
    assert!(!masked.contains("QUJDREVG"), "{masked:?}");
    assert!(masked.contains("after"), "{masked:?}");
    assert!(tail.redacted);
}

// -- truncated says which way ---------------------------------------------------

#[test]
fn a_tail_says_whether_it_was_clipped_or_rotated_or_both() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record(&store, "term-1-1", &stream(200), 50);
    let (_, clipped) = tail(store.read("term-1-1", 64, false, None));
    assert!(clipped.truncated && clipped.clipped && !clipped.rotated);
    let (_, whole) = tail(store.read("term-1-1", 4096, false, None));
    assert!(!whole.truncated && !whole.clipped && !whole.rotated);

    record(&store, "term-1-2", &stream(5000), 100);
    let (_, rotated) = tail(store.read("term-1-2", 4096, false, None));
    assert!(rotated.truncated && rotated.rotated && !rotated.clipped);
    let (_, both) = tail(store.read("term-1-2", 64, false, None));
    assert!(both.truncated && both.rotated && both.clipped);
}

// -- a rotation while a read is under way -------------------------------------

/// Numbered lines, so a read that skipped a stretch in the middle is caught
/// where the periodic `stream` would hide it.
fn lines(from: usize, to: usize) -> Vec<u8> {
    (from..to)
        .flat_map(|n| format!("line {n:05}\r\n").into_bytes())
        .collect()
}

#[test]
fn a_rotation_between_the_two_opens_of_a_read_does_not_drop_a_segment_from_the_middle() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    // 12 bytes a line, 256 to a segment: 40 lines have rotated twice.
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(&lines(0, 40));
    assert!(store.flush(Duration::from_secs(5)));
    let mut rotated_once = false;
    let read = store
        .inner
        .read_tail_between("term-1-1", 4096, &mut || {
            if !rotated_once {
                rotated_once = true;
                // What the writer does with the next full segment, while the
                // reader holds the older one open and has yet to open the newer.
                tap.chunk(&lines(40, 60));
                assert!(store.flush(Duration::from_secs(5)));
            }
        })
        .unwrap()
        .unwrap();
    assert!(rotated_once);
    let text = String::from_utf8(read.bytes).unwrap();
    let numbers: Vec<usize> = text
        .lines()
        .filter_map(|l| l.strip_prefix("line ")?.parse().ok())
        .collect();
    assert!(numbers.len() > 10, "{text:?}");
    for pair in numbers.windows(2) {
        assert_eq!(
            pair[1],
            pair[0] + 1,
            "lines are missing from the middle of {numbers:?}"
        );
    }
    drop(tap);
}

// -- the directory scan says when it stopped early ----------------------------

#[test]
fn a_directory_scan_that_hit_its_cap_says_so() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    for n in 0..5 {
        std::fs::write(files(&dir).join(format!("term-1-{n}.log")), b"x").unwrap();
    }
    let (held, capped) = store.inner.scan_up_to(3).unwrap();
    assert!(capped, "five entries, three read");
    assert!(held.len() <= 3);
    let (held, capped) = store.inner.scan_up_to(5).unwrap();
    assert!(!capped, "all five were read");
    assert_eq!(held.len(), 5);
}

#[test]
fn a_failure_note_is_part_of_its_sessions_files_for_retention() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    std::fs::write(files(&dir).join("term-1-1.log.err"), b"disk full").unwrap();
    std::fs::write(files(&dir).join("term-1-1.log"), b"x").unwrap();
    let (held, _) = store.inner.scan_up_to(10).unwrap();
    assert_eq!(held["term-1-1"].paths.len(), 2);
    assert_eq!(held["term-1-1"].bytes, 10);
    store.inner.remove_files("term-1-1");
    assert!(names(&files(&dir)).is_empty());
}

// -- the writer panicking -----------------------------------------------------

#[test]
fn a_writer_that_panics_marks_the_session_failed_and_does_not_hold_up_a_read() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    *lock(&store.inner.panic_on) = Some("term-1-1".into());
    let tap = store.arm("term-1-1", None).unwrap();
    tap.chunk(b"never written");
    let started = std::time::Instant::now();
    let read = store.read("term-1-1", 4096, false, None);
    assert!(
        started.elapsed() < SETTLE_DEADLINE / 2,
        "a read waited {:?} for operations that can no longer finish",
        started.elapsed()
    );
    let (_, tail) = tail(read);
    assert!(tail.settled);
    let error = tail.recording_error.expect("the session's output was lost");
    assert!(error.contains("stopped unexpectedly"), "{error}");
}

#[test]
fn a_marker_the_panicked_writer_never_appended_stops_holding_its_bytes_back() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1", None).unwrap();
    assert!(store.flush(Duration::from_secs(5)), "the session opens");
    *lock(&store.inner.panic_on) = Some("term-1-1".into());
    {
        // One batch for the writer: the data it panics on, then a marker it
        // never gets to.
        let mut queue = lock(&store.inner.queue);
        queue.push(Op::Data("term-1-1".into(), b"never written".to_vec()), 0);
        queue.gaps.insert("term-1-1".into(), 500);
        queue.mark_gap("term-1-1", 0);
        assert_eq!(queue.unrecorded("term-1-1"), 500);
    }
    store.inner.work.notify_one();
    assert!(store.flush(Duration::from_secs(5)), "the batch finishes");
    assert_eq!(
        lock(&store.inner.queue).unrecorded("term-1-1"),
        0,
        "a marker nobody will write still holds its bytes as pending"
    );
    let (_, tail) = tail(store.read("term-1-1", 4096, false, None));
    assert!(tail.recording_error.is_some(), "the loss is still reported");
    drop(tap);
}

// -- age, in a host that stays up ---------------------------------------------

#[test]
fn a_host_that_stays_up_ages_out_old_transcripts_whenever_a_session_ends() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record(&store, "term-1-1", b"old", 3);
    assert!(store.flush(Duration::from_secs(5)));
    age(
        &files(&dir).join("term-1-1.log"),
        Duration::from_secs(15 * 24 * 3600),
    );
    record(&store, "term-1-2", b"new", 3);
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(names(&files(&dir)), ["term-1-2.log"]);
}

// -- terminal ids repeat: a transcript is a run's, not just an id's -----------

/// A whole session spawned for `run`, ended and written out.
fn record_for(store: &Transcripts, session: &str, run: Option<&str>, bytes: &[u8]) {
    let tap = store.arm(session, run).unwrap();
    tap.chunk(bytes);
    drop(tap);
    assert!(store.flush(Duration::from_secs(5)));
}

fn is_reused(read: &TranscriptRead) -> bool {
    matches!(read, TranscriptRead::Reused { .. })
}

#[test]
fn a_later_session_under_a_reused_id_does_not_delete_the_earlier_runs_transcript() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record_for(&store, "term-1-1", Some("run-a"), b"the first run's output");
    record_for(
        &store,
        "term-1-1",
        Some("run-b"),
        b"the second run's output",
    );
    // Each run reads its own, though they share one terminal id.
    let (second, tail_b) = tail(store.read("term-1-1", 100, false, Some("run-b")));
    assert_eq!(second, b"the second run's output");
    assert_eq!(tail_b.run_id.as_deref(), Some("run-b"));
    let (first, tail_a) = tail(store.read("term-1-1", 100, false, Some("run-a")));
    assert_eq!(first, b"the first run's output");
    assert_eq!(tail_a.run_id.as_deref(), Some("run-a"));
    assert!(!tail_a.live);
    // A read that names no run gets what the id holds now, and says whose it is.
    let (now, tail_now) = tail(store.read("term-1-1", 100, false, None));
    assert_eq!(now, b"the second run's output");
    assert_eq!(tail_now.run_id.as_deref(), Some("run-b"));
    // A run the host has no transcript of under that id is not given another's.
    assert!(is_reused(&store.read(
        "term-1-1",
        100,
        false,
        Some("run-c")
    )));
}

#[test]
fn a_read_for_a_run_never_returns_another_sessions_output() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record_for(&store, "term-1-1", Some("run-b"), b"belongs to b");
    let reused = store.read("term-1-1", 100, false, Some("run-a"));
    match &reused {
        TranscriptRead::Reused { reason } => {
            assert!(reason.contains("reused this terminal id"), "{reason}")
        }
        other => panic!("another run's output was served for run-a: {other:?}"),
    }
    assert_eq!(wire(reused)["state"], "reused");
    // A session spawned for no run is not this run's either, and the answer
    // does not claim to have seen a reuse.
    record_for(&store, "term-1-2", None, b"an ordinary shell");
    match store.read("term-1-2", 100, false, Some("run-a")) {
        TranscriptRead::Reused { reason } => {
            assert!(reason.contains("no run was recorded"), "{reason}")
        }
        other => panic!("a shell's output was served for run-a: {other:?}"),
    }
    // Nothing under the id at all is missing, not reused.
    assert_eq!(
        store.read("term-1-3", 100, false, Some("run-a")),
        TranscriptRead::Missing
    );
}

#[test]
fn a_session_replaces_what_its_own_run_or_no_run_left_but_keeps_what_another_run_left() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    // The same run again: replaced, nothing kept.
    record_for(&store, "term-1-1", Some("run-a"), b"first");
    record_for(&store, "term-1-1", Some("run-a"), b"second");
    assert_eq!(
        names(&files(&dir)),
        ["term-1-1.log", "term-1-1.log.meta"],
        "the same run left nothing behind"
    );
    // An earlier session with no run is replaced, as ever.
    record_for(&store, "term-1-2", None, b"shell");
    record_for(&store, "term-1-2", Some("run-a"), b"a run");
    assert_eq!(
        names(&files(&dir))
            .iter()
            .filter(|n| n.starts_with("term-1-2"))
            .count(),
        2,
        "a session with no run left nothing worth keeping: {:?}",
        names(&files(&dir))
    );
    // A run's output followed by a session with no run is kept.
    record_for(&store, "term-1-2", None, b"shell again");
    let kept = kept_stem("term-1-2", "run-a");
    assert!(
        files(&dir).join(format!("{kept}.log")).exists(),
        "{:?}",
        names(&files(&dir))
    );
    assert_eq!(
        tail(store.read("term-1-2", 100, false, Some("run-a"))).0,
        b"a run"
    );
}

#[test]
fn a_keep_that_fails_part_way_leaves_the_kept_name_with_the_record_of_whose_it_is() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record_for(&store, "term-1-1", Some("run-a"), b"a run's output");
    // The log cannot move onto a directory with something in it.
    let kept = kept_stem("term-1-1", "run-a");
    let blocker = files(&dir).join(format!("{kept}.log"));
    std::fs::create_dir(&blocker).unwrap();
    std::fs::write(blocker.join("occupied"), b"x").unwrap();
    record_for(&store, "term-1-1", Some("run-b"), b"the next run");
    assert!(
        files(&dir).join(format!("{kept}.log.meta")).exists(),
        "the record was left behind, so what is kept cannot say whose it is: {:?}",
        names(&files(&dir))
    );
}

#[test]
fn a_kept_transcript_is_held_to_the_directory_limits_under_its_own_name() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record_for(&store, "term-1-1", Some("run-a"), b"kept for a while");
    record_for(&store, "term-1-1", Some("run-b"), b"the current one");
    let kept = kept_stem("term-1-1", "run-a");
    assert!(is_stem(&kept) && !crate::terminal::is_session_id(&kept));
    let (held, capped) = store.inner.scan_up_to(MAX_SCANNED).unwrap();
    assert!(!capped);
    assert!(
        held.get(&kept).is_some_and(|entry| entry.paths.len() == 2),
        "the kept log and its record are accounted under their own name: {:?}",
        held.keys().collect::<Vec<_>>()
    );
    assert_eq!(
        held["term-1-1"].paths.len(),
        2,
        "and the id's own files are only the current run's"
    );
    // It ages out like any finished transcript, while the current run stays.
    for suffix in ["log", "log.meta"] {
        age(
            &files(&dir).join(format!("{kept}.{suffix}")),
            Duration::from_secs(15 * 24 * 3600),
        );
    }
    store.inner.sweep(&HashSet::new()).unwrap();
    assert!(is_reused(&store.read(
        "term-1-1",
        100,
        false,
        Some("run-a")
    )));
    assert_eq!(
        tail(store.read("term-1-1", 100, false, Some("run-b"))).0,
        b"the current one"
    );
}

#[test]
fn the_run_record_is_private_and_a_link_in_its_place_is_unreadable() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    record_for(&store, "term-1-1", Some("run-a"), b"output");
    let meta = files(&dir).join("term-1-1.log.meta");
    assert_eq!(std::fs::read_to_string(&meta).unwrap(), "run-a");
    assert_eq!(
        std::fs::metadata(&meta).unwrap().permissions().mode() & 0o777,
        0o600
    );
    std::fs::remove_file(&meta).unwrap();
    let target = dir.path().join("elsewhere");
    std::fs::write(&target, "run-a").unwrap();
    symlink(&target, &meta).unwrap();
    // Not followed, so not taken as proof the output is run-a's.
    match store.read("term-1-1", 100, false, Some("run-a")) {
        TranscriptRead::Unreadable { reason } => {
            assert!(reason.contains("could not be read"), "{reason}")
        }
        other => panic!("a linked run record was trusted: {other:?}"),
    }
}

#[test]
fn a_run_id_too_long_to_keep_is_clipped_the_same_way_everywhere() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let long = "r".repeat(RUN_ID_MAX + 100);
    record_for(&store, "term-1-1", Some(&long), b"output");
    let (_, tail) = tail(store.read("term-1-1", 100, false, Some(&long)));
    assert_eq!(tail.run_id.as_deref(), Some(clip_run_id(&long)));
}
