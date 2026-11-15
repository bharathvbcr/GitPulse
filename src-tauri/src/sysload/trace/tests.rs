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
