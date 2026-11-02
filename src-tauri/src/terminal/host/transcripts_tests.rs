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
    let tap = store.arm(session).unwrap();
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
    let running = store.arm("term-1-1").unwrap();
    for piece in expected.chunks(37) {
        running.chunk(piece);
    }
    let (got, tail) = tail(store.read("term-1-1", 4096, false));
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
    let tap = store.arm("term-1-1").unwrap();
    tap.chunk(b"hello\r\n");
    drop(tap);
    let (got, tail) = tail(store.read("term-1-1", 4096, false));
    assert_eq!(got, b"hello\r\n");
    assert!(!tail.live);
}

#[test]
fn a_full_segment_becomes_the_previous_one_and_nothing_is_lost_until_the_second_rotation() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let expected = stream(400);
    record(&store, "term-1-1", &expected, 90);
    let (got, tail) = tail(store.read("term-1-1", 4096, false));
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
    let (got, tail) = tail(store.read("term-1-1", 4096, false));
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
    let (got, tail) = tail(store.read("term-1-1", 64, false));
    assert_eq!(got, &expected[136..]);
    assert!(tail.truncated, "clipped by the bound");
    assert_eq!(tail.retained, 200);
    assert_eq!(tail.bytes, 64);
}

#[test]
fn a_session_with_no_transcript_is_missing_and_a_bad_name_is_not() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    assert_eq!(store.read("term-9-9", 100, false), TranscriptRead::Missing);
    for bad in ["../x", "term-1-a/../../y", "", "term-x-1"] {
        match store.read(bad, 100, false) {
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
    match store.read("term-1-1", 100, false) {
        TranscriptRead::Unreadable { reason } => {
            assert!(reason.contains("symbolic link"), "{reason}");
            assert!(!reason.contains("not a transcript"));
        }
        other => panic!("a link read as {other:?}"),
    }
    symlink(&outside, files(&dir).join("term-1-2.log.1")).unwrap();
    assert!(matches!(
        store.read("term-1-2", 100, false),
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
    let (got, _) = tail(store.read("term-1-1", 100, false));
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
    let tap = store.arm("term-1-1").unwrap();
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
    let (got, tail) = tail(store.read("term-1-1", MAX_TRANSCRIPT_TAIL_BYTES, false));
    assert!(tail.gap_markers >= 1, "{} markers", tail.gap_markers);
    let text = String::from_utf8(got).unwrap();
    let mut recorded = 0u64;
    let mut dropped = 0u64;
    let mut rest = text.as_str();
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
    assert_eq!(
        recorded + dropped,
        200 * 300,
        "every byte is either in the log or counted in a marker"
    );
    assert!(dropped > 0 && recorded > 0);
}

#[test]
fn a_directory_over_its_total_drops_the_oldest_finished_session_and_never_a_running_one() {
    let dir = private_tempdir();
    let store = store(
        &dir,
        Limits {
            total_bytes: 650,
            ..small()
        },
    );
    let running = store.arm("term-1-9").unwrap();
    running.chunk(&stream(200));
    for session in ["term-1-1", "term-1-2"] {
        record(&store, session, &stream(200), 200);
    }
    assert!(store.flush(Duration::from_secs(5)));
    assert_eq!(names(&files(&dir)).len(), 3, "600 bytes fit in 650");
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
    assert_eq!(store.read("term-1-1", 100, false), TranscriptRead::Missing);
    assert_eq!(tail(store.read("term-1-2", 100, false)).0, b"recent");
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
    let (got, tail) = tail(restarted.read("term-1-1", 4096, false));
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
    assert_eq!(tail(store.read("term-1-1", 100, false)).0, b"this one");
}

#[test]
fn a_session_that_never_started_leaves_nothing_but_a_running_one_is_never_discarded() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let tap = store.arm("term-1-1").unwrap();
    tap.chunk(b"live");
    store.discard("term-1-1");
    assert_eq!(tail(store.read("term-1-1", 100, false)).0, b"live");
    drop(tap);
    store.discard("term-1-1");
    assert_eq!(store.read("term-1-1", 100, false), TranscriptRead::Missing);
}

#[test]
fn a_session_is_recorded_once_at_a_time() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    let first = store.arm("term-1-1").unwrap();
    assert!(matches!(store.arm("term-1-1"), Err(ArmError::Duplicate)));
    drop(first);
    assert!(store.arm("term-1-1").is_ok());
}

#[test]
fn a_write_that_cannot_happen_is_reported_not_called_missing() {
    let dir = private_tempdir();
    let store = store(&dir, small());
    std::fs::remove_dir_all(files(&dir)).unwrap();
    record(&store, "term-1-1", b"lost", 4);
    match store.read("term-1-1", 100, false) {
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
    let (raw, plain) = tail(store.read("term-1-1", 4096, false));
    assert!(String::from_utf8(raw).unwrap().contains(secret));
    assert!(!plain.redacted);
    let (masked, tail) = tail(store.read("term-1-1", 4096, true));
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
    assert!(matches!(store.arm("term-1-1"), Err(ArmError::Closed)));
}
