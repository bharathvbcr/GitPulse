use super::*;
use crate::sysload::sensor::{CompressorSegments, HostSample};
use crate::sysload::snapshot::WriterIdentity;
use crate::sysload::{Clock, Platform, SessionCount, Why};

fn rec(n: u64) -> TraceRecord {
    TraceRecord {
        wall_s: 1_700_000_000 + n * 10,
        mono_ms: n * 10_000,
        boot_id: 7,
        pressure: Some(Level::Warn),
        tier: Some(Tier::Elevated),
        binding: Some(Binding::Swap),
        admit: Some(true),
        swap_used: Some(n << 30),
        compressed_bytes: Some(n << 20),
        load_1m: Some(n as f64 + 0.5),
        free_disk: Some(100 << 30),
        launched_sessions: Some(n as u32),
        observed_sessions: Some(2),
        largest_foreign: Some(n << 25),
        launched_footprint: Some(n << 26),
        thermal: Some(Thermal::Fair),
        power: Some(Power::Ac),
        segment_fraction: Some(0.25),
        dropped_before: 0,
        dropped_total: 0,
    }
}

fn blank(n: u64) -> TraceRecord {
    TraceRecord {
        wall_s: n,
        mono_ms: n,
        boot_id: 0,
        pressure: None,
        tier: None,
        binding: None,
        admit: None,
        swap_used: None,
        compressed_bytes: None,
        load_1m: None,
        free_disk: None,
        launched_sessions: None,
        observed_sessions: None,
        largest_foreign: None,
        launched_footprint: None,
        thermal: None,
        power: None,
        segment_fraction: None,
        dropped_before: 0,
        dropped_total: 0,
    }
}

fn path(dir: &tempfile::TempDir) -> PathBuf {
    dir.path().join(TRACE_FILE_NAME)
}

#[test]
fn a_full_ring_overwrites_its_oldest_records_and_never_grows() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = TraceWriter::open_with_capacity(dir.path(), 7, 4).unwrap();
    for n in 1..=10 {
        writer.append(&rec(n)).unwrap();
    }
    assert_eq!(
        std::fs::metadata(path(&dir)).unwrap().len() as usize,
        file_len(4)
    );
    let held = read_ring(&path(&dir)).unwrap();
    assert_eq!(
        held.iter().map(|r| r.wall_s).collect::<Vec<_>>(),
        [7, 8, 9, 10].map(|n| 1_700_000_000 + n * 10),
        "the newest four, oldest first"
    );
    assert_eq!(held[3], rec(10));
}

#[test]
fn the_real_capacity_is_three_hours_at_ten_second_records() {
    assert_eq!(u64::from(CAPACITY) * EVERY_NTH_TICK * (crate::sysload::CADENCE_MS / 1000), 3 * 3600);
    assert_eq!(file_len(CAPACITY), 64 + 1080 * 128);
}

#[test]
fn an_unreadable_reading_comes_back_unreadable_and_a_real_zero_stays_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = TraceWriter::open_with_capacity(dir.path(), 7, 4).unwrap();
    let zeros = TraceRecord {
        swap_used: Some(0),
        load_1m: Some(0.0),
        free_disk: Some(0),
        launched_sessions: Some(0),
        segment_fraction: Some(0.0),
        binding: None,
        admit: Some(false),
        tier: Some(Tier::Clear),
        ..blank(2)
    };
    writer.append(&blank(1)).unwrap();
    writer.append(&zeros).unwrap();
    let held = read_ring(&path(&dir)).unwrap();
    assert_eq!(held[0], blank(1));
    assert_eq!(held[0].swap_used, None);
    assert_eq!(held[1], zeros);
    assert_eq!(held[1].swap_used, Some(0), "zero is a reading");
    assert_eq!(held[1].admit, Some(false));
}

#[test]
fn a_new_boot_keeps_the_old_ring_as_prev_and_replaces_an_older_prev() {
    let dir = tempfile::tempdir().unwrap();
    let prev = dir.path().join(PREV_TRACE_FILE_NAME);
    let mut first = TraceWriter::open_with_capacity(dir.path(), 1, 4).unwrap();
    first.append(&rec(1)).unwrap();
    drop(first);
    assert!(!prev.exists());

    let mut second = TraceWriter::open_with_capacity(dir.path(), 2, 4).unwrap();
    assert_eq!(read_ring(&prev).unwrap(), [rec(1)], "boot 1's readings survive");
    assert_eq!(read_ring(&path(&dir)).unwrap(), [], "boot 2 starts empty");
    second.append(&rec(2)).unwrap();
    drop(second);

    let _third = TraceWriter::open_with_capacity(dir.path(), 3, 4).unwrap();
    assert_eq!(read_ring(&prev).unwrap(), [rec(2)], "an older prev is replaced");
}

#[test]
fn the_same_boot_carries_the_ring_on() {
    let dir = tempfile::tempdir().unwrap();
    let mut a = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
    a.append(&rec(1)).unwrap();
    a.append(&rec(2)).unwrap();
    drop(a);
    let mut b = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
    assert_eq!(b.next_seq(), 3);
    b.append(&rec(3)).unwrap();
    assert_eq!(read_ring(&path(&dir)).unwrap(), [rec(1), rec(2), rec(3)]);
    assert!(!dir.path().join(PREV_TRACE_FILE_NAME).exists());
}

#[test]
fn a_torn_short_or_foreign_file_is_a_typed_error_never_decoded_and_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    let prev = dir.path().join(PREV_TRACE_FILE_NAME);
    let mut good = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
    for n in 1..=4 {
        good.append(&rec(n)).unwrap();
    }
    drop(good);
    let whole = std::fs::read(&file).unwrap();

    // Short: cut in the middle of a record.
    std::fs::write(&file, &whole[..whole.len() - 50]).unwrap();
    assert!(matches!(read_ring(&file), Err(TraceError::Short { .. })));
    // Foreign: wrong magic; and a header that fails its checksum.
    let mut foreign = whole.clone();
    foreign[0] = b'X';
    std::fs::write(&file, &foreign).unwrap();
    assert_eq!(read_ring(&file), Err(TraceError::BadMagic));
    let mut damaged = whole.clone();
    damaged[H_BOOT] ^= 0xFF;
    std::fs::write(&file, &damaged).unwrap();
    assert_eq!(read_ring(&file), Err(TraceError::BadHeader));
    let mut newer = whole.clone();
    put_u32(&mut newer, H_VERSION, 9);
    std::fs::write(&file, &newer).unwrap();
    assert_eq!(read_ring(&file), Err(TraceError::UnsupportedVersion(9)));
    // Not even a header.
    std::fs::write(&file, b"GPH").unwrap();
    assert!(matches!(read_ring(&file), Err(TraceError::Short { .. })));

    // Each is replaced by the next writer, and none is kept as the previous
    // boot's trace, because none was decoded.
    for bad in [&whole[..100], &foreign[..], &damaged[..], &newer[..]] {
        std::fs::write(&file, bad).unwrap();
        let mut writer = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
        writer.append(&rec(1)).unwrap();
        assert_eq!(read_ring(&file).unwrap(), [rec(1)]);
        assert!(!prev.exists(), "an unreadable file is not a previous boot");
    }
}

#[test]
fn a_record_torn_by_a_crash_is_dropped_and_its_neighbours_survive() {
    let dir = tempfile::tempdir().unwrap();
    let mut writer = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
    for n in 1..=3 {
        writer.append(&rec(n)).unwrap();
    }
    let file = path(&dir);
    let mut bytes = std::fs::read(&file).unwrap();
    // Half of record 2 was written before the crash.
    let at = HEADER_LEN + RECORD_LEN;
    bytes[at + 60..at + RECORD_LEN].fill(0xAB);
    std::fs::write(&file, &bytes).unwrap();
    assert_eq!(read_ring(&file).unwrap(), [rec(1), rec(3)]);
}

#[cfg(unix)]
#[test]
fn a_trace_this_user_does_not_alone_own_is_refused_and_left_alone() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let file = path(&dir);
    let mut writer = TraceWriter::open_with_capacity(dir.path(), 5, 4).unwrap();
    writer.append(&rec(1)).unwrap();
    drop(writer);
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    let error = TraceWriter::open_with_capacity(dir.path(), 5, 4).err().unwrap();
    assert!(error.contains("other users"), "{error}");
    assert!(matches!(read_ring(&file), Err(TraceError::Unsafe(_))));
    assert!(file.exists(), "a refused file is not replaced");
    std::fs::remove_file(&file).unwrap();

    let target = dir.path().join("elsewhere");
    std::fs::write(&target, b"x").unwrap();
    std::os::unix::fs::symlink(&target, &file).unwrap();
    assert!(TraceWriter::open_with_capacity(dir.path(), 5, 4).is_err());
    assert_eq!(std::fs::read(&target).unwrap(), b"x", "nothing written through the link");
}

#[test]
fn the_previous_boot_is_its_last_thirty_records_with_their_peaks() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(previous_boot(dir.path()), None, "no previous trace is not an error");
    let mut writer = TraceWriter::open_with_capacity(dir.path(), 1, 100).unwrap();
    for n in 1..=50 {
        let mut r = rec(n);
        r.admit = Some(n != 49);
        r.free_disk = Some(if n == 20 { 1 << 30 } else { 100 << 30 });
        writer.append(&r).unwrap();
    }
    drop(writer);
    drop(TraceWriter::open_with_capacity(dir.path(), 2, 100).unwrap());
    let before = previous_boot(dir.path()).unwrap();
    assert_eq!(before.error, None);
    assert_eq!(before.records.len(), PREVIOUS_TAIL);
    assert_eq!(before.records[0], {
        let mut r = rec(21);
        r.free_disk = Some(100 << 30);
        r
    });
    assert_eq!(before.ended_wall_s, Some(rec(50).wall_s));
    let peaks = &before.peaks;
    assert_eq!(peaks.records, PREVIOUS_TAIL);
    assert_eq!(peaks.max_swap_used, Some(50 << 30));
    assert_eq!(peaks.max_load_1m, Some(50.5));
    assert_eq!(peaks.min_free_disk, Some(100 << 30), "record 20 is outside the tail");
    assert!(peaks.refused, "record 49 refused launches");
    assert_eq!(peaks.max_tier, Some(Tier::Elevated));
    assert_eq!(peaks.worst_thermal, Some(Thermal::Fair));
}

#[test]
fn peaks_of_unreadable_readings_are_unknown_not_zero() {
    let peaks = TracePeaks::of(&[blank(1), blank(2)]);
    assert_eq!(peaks.records, 2);
    assert_eq!(peaks.max_swap_used, None);
    assert_eq!(peaks.min_free_disk, None);
    assert_eq!(peaks.max_tier, None);
    assert!(!peaks.refused);
}

#[test]
fn a_previous_trace_that_cannot_be_trusted_is_said_not_shown_empty() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join(PREV_TRACE_FILE_NAME), b"not a trace at all").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.path().join(PREV_TRACE_FILE_NAME),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
    let before = previous_boot(dir.path()).unwrap();
    assert!(before.error.is_some());
    assert!(before.records.is_empty());
    assert_eq!(before.ended_wall_s, None);
}

#[test]
fn only_the_elected_writer_measuring_on_every_fifth_tick_appends() {
    let ticks: Vec<u64> = (1..=20).filter(|t| due(true, true, *t)).collect();
    assert_eq!(ticks, [5, 10, 15, 20]);
    assert!(!(1..=20).any(|t| due(false, true, t)), "a reader never appends");
    assert!(!(1..=20).any(|t| due(true, false, t)), "switched off records nothing");
}

#[test]
fn a_record_is_made_from_the_snapshot_and_the_held_verdict() {
    let mut snap = Snapshot::empty(
        Clock {
            wall_ms: 1_700_000_123_456,
            mono_ns: 9_000_000_000,
            boot_id: 42,
        },
        WriterIdentity::default(),
        true,
    );
    snap.platform = Platform::Governed;
    snap.host = HostSample {
        pressure: Reading::Value(Level::Critical),
        swap_used: Reading::Value(57 << 30),
        load: Reading::Value([120.0, 90.0, 40.0]),
        compressor_segments: Reading::Value(CompressorSegments {
            total: 1000,
            limit: 4000,
        }),
        thermal: Reading::Unavailable(Why::Unreadable),
        ..HostSample::default()
    };
    snap.claude = Reading::Value(SessionCount {
        launched: 3,
        observed: 4,
    });
    snap.codex = Reading::Value(SessionCount {
        launched: 1,
        observed: 0,
    });
    let r = TraceRecord::of(&snap, Tier::Severe, Some(Binding::Swap), false);
    assert_eq!((r.wall_s, r.mono_ms, r.boot_id), (1_700_000_123, 9_000, 42));
    assert_eq!(r.pressure, Some(Level::Critical));
    assert_eq!((r.tier, r.binding, r.admit), (Some(Tier::Severe), Some(Binding::Swap), Some(false)));
    assert_eq!(r.swap_used, Some(57 << 30));
    assert_eq!(r.load_1m, Some(120.0));
    assert_eq!((r.launched_sessions, r.observed_sessions), (Some(4), Some(4)));
    assert_eq!(r.segment_fraction, Some(0.25));
    assert_eq!(r.thermal, None, "unreadable is not nominal");
    assert_eq!(r.free_disk, None);
    assert_eq!(r.largest_foreign, None);
    // One session count unreadable makes the sum unknown, not a partial one.
    snap.codex = Reading::Unavailable(Why::Unreadable);
    let r = TraceRecord::of(&snap, Tier::Clear, None, true);
    assert_eq!((r.launched_sessions, r.observed_sessions), (None, None));
}

const SECONDS: Duration = Duration::from_secs(1);

fn feed_over(gate: &std::sync::Arc<blocked_sink_tests::Gate>, dir: &tempfile::TempDir) -> TraceFeed {
    TraceFeed::start(blocked_sink_tests::blocked_writes(gate.clone()), dir.path().to_path_buf(), 7).unwrap()
}

/// Records offered while the write is stuck are dropped, counted, and the
/// count reaches the ring in the next record that lands: a ring with a gap
/// says it has one.
#[test]
fn records_offered_while_the_write_is_blocked_are_dropped_and_the_ring_says_how_many() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    let mut feed = feed_over(&gate, &dir);
    assert!(feed.offer(rec(1)));
    assert!(gate.wait_entered(30 * SECONDS), "the thread never reached the sink");
    assert!(feed.offer(rec(2)), "the slot is free once the thread took the first");
    assert!(!feed.offer(rec(3)));
    assert!(!feed.offer(rec(4)));
    assert_eq!(feed.dropped(), 2);
    gate.release();
    assert!(feed.stop(30 * SECONDS), "stopped once the write resumed");
    let held = read_ring(&path(&dir)).unwrap();
    assert_eq!(held.len(), 2, "records 3 and 4 are not in the ring");
    assert_eq!((held[0].wall_s, held[0].dropped_before, held[0].dropped_total), (rec(1).wall_s, 0, 0));
    assert_eq!(
        (held[1].wall_s, held[1].dropped_before, held[1].dropped_total),
        (rec(2).wall_s, 2, 2),
        "the record after the gap carries the count"
    );
}

/// The tick's call never waits on the file: a thousand offers against a stuck
/// write return at once.
#[test]
fn offering_never_waits_on_a_blocked_write() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    let mut feed = feed_over(&gate, &dir);
    assert!(feed.offer(rec(1)));
    assert!(gate.wait_entered(30 * SECONDS));
    let began = Instant::now();
    for n in 2..1_002 {
        feed.offer(rec(n));
    }
    assert!(
        began.elapsed() < 5 * SECONDS,
        "1000 offers took {:?} behind a blocked write",
        began.elapsed()
    );
    assert_eq!(feed.dropped(), 999);
    gate.release();
    feed.stop(30 * SECONDS);
}

/// `stop` is bounded by the wait it is given: a write blocked in the kernel
/// cannot be interrupted, so the thread is detached, still owns the file, and
/// finishes its writes once the disk answers.
#[test]
fn stop_returns_after_its_wait_with_a_blocked_write_and_the_detached_thread_finishes_later() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    let mut feed = feed_over(&gate, &dir);
    assert!(feed.offer(rec(1)));
    assert!(gate.wait_entered(30 * SECONDS));
    assert!(feed.offer(rec(2)));
    let began = Instant::now();
    let finished = feed.stop(Duration::from_millis(150));
    let took = began.elapsed();
    assert!(!finished, "the write is still blocked");
    assert!(took >= Duration::from_millis(100), "waited its bound: {took:?}");
    assert!(took < 10 * SECONDS, "returned in {took:?}, not when the write did");
    gate.release();
    let deadline = Instant::now() + 30 * SECONDS;
    loop {
        let held = read_ring(&path(&dir)).map(|r| r.len()).unwrap_or(0);
        if held == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "the detached thread never wrote its records ({held})");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn stopping_a_feed_that_is_not_blocked_joins_and_writes_the_waiting_record() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    gate.release();
    let mut feed = feed_over(&gate, &dir);
    assert!(feed.offer(rec(1)));
    assert!(feed.stop(30 * SECONDS));
    assert_eq!(read_ring(&path(&dir)).unwrap().len(), 1);
    assert!(!feed.offer(rec(2)), "a stopped feed takes nothing");
}

/// An open that fails is a problem the sampler can say, retried later, and
/// does not keep `stop` waiting.
#[test]
fn a_failing_open_is_a_problem_and_does_not_hold_up_stop() {
    let opener: TraceOpener = Arc::new(|_, _| Err("disk said no".into()));
    let dir = tempfile::tempdir().unwrap();
    let mut feed = TraceFeed::start(opener, dir.path().to_path_buf(), 7).unwrap();
    let deadline = Instant::now() + 30 * SECONDS;
    while feed.problem().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(feed.problem().as_deref(), Some("trace: disk said no"));
    assert!(!feed.take_opened());
    let began = Instant::now();
    assert!(feed.stop(30 * SECONDS));
    assert!(began.elapsed() < 5 * SECONDS, "stop waited for the reopen timer");
}

/// A write the disk refused loses its record as surely as a full slot does, so
/// it is counted the same way: the next record that lands carries it, and
/// `dropped()` includes it. A write that failed must not look like one that
/// landed.
#[test]
fn a_write_that_failed_is_counted_as_a_lost_record() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    gate.release();
    let opener = blocked_sink_tests::failing_writes(gate, 1);
    let mut feed = TraceFeed::start(opener, dir.path().to_path_buf(), 7).unwrap();
    assert!(feed.offer(rec(1)));
    let deadline = Instant::now() + 30 * SECONDS;
    while feed.problem().is_none() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(feed.problem().as_deref(), Some("trace: disk said no"));
    assert!(feed.offer(rec(2)));
    assert!(feed.stop(30 * SECONDS));
    let held = read_ring(&path(&dir)).unwrap();
    assert_eq!(held.len(), 1, "only the second record landed");
    assert_eq!(held[0].wall_s, rec(2).wall_s);
    assert_eq!(held[0].dropped_before, 1, "the failed write is in the gap");
    assert_eq!(held[0].dropped_total, 1);
    assert_eq!(feed.dropped(), 1);
}

/// A full slot and a failed write in the same gap add up.
#[test]
fn a_dropped_record_and_a_failed_write_in_one_gap_are_both_counted() {
    let dir = tempfile::tempdir().unwrap();
    let gate = blocked_sink_tests::Gate::new();
    let opener = blocked_sink_tests::failing_writes(gate.clone(), 1);
    let mut feed = TraceFeed::start(opener, dir.path().to_path_buf(), 7).unwrap();
    assert!(feed.offer(rec(1)));
    assert!(gate.wait_entered(30 * SECONDS));
    assert!(feed.offer(rec(2)));
    assert!(!feed.offer(rec(3)), "the slot is full: dropped");
    gate.release();
    assert!(feed.stop(30 * SECONDS));
    let held = read_ring(&path(&dir)).unwrap();
    assert_eq!(held.len(), 1, "record 1 failed, record 3 was dropped");
    assert_eq!(held[0].wall_s, rec(2).wall_s);
    assert_eq!(held[0].dropped_before, 2, "one dropped plus one failed");
    assert_eq!(held[0].dropped_total, 2);
    assert_eq!(feed.dropped(), 2);
}

/// The count the ring carries is summed over everything it holds when the
/// previous boot is read, and a record an older build wrote (zeros there)
/// reads as nothing dropped.
#[test]
fn the_previous_boot_reports_the_drops_its_ring_recorded() {
    let dir = tempfile::tempdir().unwrap();
    {
        let mut old = TraceWriter::open(dir.path(), 7).unwrap();
        old.append(&rec(1)).unwrap();
        old.append(&TraceRecord { dropped_before: 3, dropped_total: 3, ..rec(2) }).unwrap();
        old.append(&TraceRecord { dropped_before: 4, dropped_total: 7, ..rec(3) }).unwrap();
    }
    TraceWriter::open(dir.path(), 8).unwrap();
    let previous = previous_boot(dir.path()).expect("the old ring is kept");
    assert_eq!(previous.dropped, 7);
    assert_eq!(previous.records[0].dropped_before, 0, "no drops reads as zero");
    assert_eq!(previous.records[2].dropped_total, 7);
}
