//! W3c adversary pass on the app's half of the durable record of notify
//! replies the hook stopped waiting for (ft-6d8ff969, item 4): the reader in
//! `alerts::status()`, its rotation and what it does with hostile or merely
//! unlucky files.
//!
//! A test whose name starts `adv_w3c_broke_` demonstrates a defect on the lane
//! tip and FAILS there; every other test here passes and adds coverage.

use crate::hooks::notify_drops::{
    prev_path, read_record, record_path, DropRecord, READ_CAP_BYTES, ROTATE_BYTES,
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

/// Enough whole lines to pass the 32 KiB rotation threshold.
fn big_record(at: u64) -> (String, usize) {
    let mut text = String::new();
    let mut lines = 0;
    while text.len() as u64 <= ROTATE_BYTES {
        text.push_str(&line(at, "session_ended"));
        lines += 1;
    }
    (text, lines)
}

/// A line whose time is the largest a `u64` holds must be read, not panic the
/// status poll while it formats the "last" time.
#[test]
fn adv_w3c_a_line_stamped_with_the_largest_time_does_not_panic_the_reader() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    std::fs::write(
        record_path(&socket),
        format!(
            "{{\"v\":1,\"at\":{},\"event\":\"session_ended\"}}\n",
            u64::MAX
        ),
    )
    .unwrap();
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 1);
    assert_eq!(report.error, None);
}

/// `.prev` as a symlink to a device that never ends: the open must refuse it
/// (no follow), say so, and come back at once.
#[cfg(unix)]
#[test]
fn adv_w3c_a_prev_that_is_a_symlink_to_dev_zero_is_an_error_not_a_read() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    std::fs::write(record_path(&socket), line(NOW - HOUR, "session_ended")).unwrap();
    std::os::unix::fs::symlink("/dev/zero", prev_path(&record_path(&socket))).unwrap();
    let started = std::time::Instant::now();
    let report = read_record(&socket, NOW);
    assert!(
        started.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(2))
    );
    assert_eq!(report.missed, 1, "the readable generation still counts");
    let error = report.error.expect("an unreadable generation is an error");
    assert!(error.contains("count may be too low"), "{error}");
}

/// `.prev` that is a directory: rotation cannot replace it. The failure must
/// be said, and the current generation must still be read and kept.
#[cfg(unix)]
#[test]
fn adv_w3c_a_directory_at_prev_blocks_rotation_loudly_and_loses_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let (text, lines) = big_record(NOW - HOUR);
    std::fs::write(record_path(&socket), &text).unwrap();
    std::fs::create_dir(prev_path(&record_path(&socket))).unwrap();
    let report = read_record(&socket, NOW);
    assert!(report.error.is_some(), "a rotation that failed was silent");
    assert_eq!(report.missed as usize, lines);
    assert_eq!(std::fs::read_to_string(record_path(&socket)).unwrap(), text);
}

/// A 1 GiB sparse record (a buggy writer, not an attacker): the reader stats it,
/// rotates by rename, refuses to read it and says so. It must not read it.
#[cfg(unix)]
#[test]
fn adv_w3c_a_sparse_gigabyte_record_is_reported_without_being_read() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let file = std::fs::File::create(record_path(&socket)).unwrap();
    file.set_len(1 << 30).unwrap();
    let started = std::time::Instant::now();
    let report = read_record(&socket, NOW);
    assert!(
        started.elapsed()
            < crate::test_support::coverage_relaxed(std::time::Duration::from_secs(2)),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.missed, 0);
    let error = report.error.expect("unknown is never shown as 0");
    assert!(error.contains("count may be too low"), "{error}");
}

/// A line that is only characters the cleaner removes must not become the
/// "last" event: the panel would say "(last:  at ...)" and an older, readable
/// event would be hidden behind it.
#[test]
fn adv_w3c_broke_an_event_that_cleans_to_nothing_is_not_shown_as_the_last_one() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let mut text = line(NOW - 2 * HOUR, "session_ended");
    text.push_str(&format!(
        "{{\"v\":1,\"at\":{},\"event\":\"\\u200b\"}}\n",
        NOW - HOUR
    ));
    std::fs::write(record_path(&socket), text).unwrap();
    let report = read_record(&socket, NOW);
    assert_eq!(report.missed, 2);
    let last = report.last.expect("a last event");
    assert!(
        last.starts_with("session_ended at "),
        "the newest line's event cleaned to nothing and replaced the readable one: {last:?}"
    );
}

/// One reader, one appender: a line that lands while the reader rotates is in
/// one of the two generations and is counted. Held to the 32 KiB threshold so
/// only a single rotation can happen.
#[cfg(unix)]
#[test]
fn adv_w3c_one_reader_rotating_under_an_appender_loses_no_line() {
    for _ in 0..200 {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(dir.path());
        let (text, preset) = big_record(NOW - HOUR);
        std::fs::write(record_path(&socket), text).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let appended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let writer = {
            let (socket, stop, appended) = (socket.clone(), stop.clone(), appended.clone());
            std::thread::spawn(move || {
                let record = DropRecord::with_clock(|| Some(NOW - 1));
                while !stop.load(std::sync::atomic::Ordering::SeqCst)
                    && appended.load(std::sync::atomic::Ordering::SeqCst) < 40
                {
                    record
                        .append(&socket, "idle_prompt", Some("term-2-1"))
                        .expect("appended");
                    appended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        let first = read_record(&socket, NOW);
        assert_eq!(first.error, None);
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        let total = read_record(&socket, NOW);
        assert_eq!(
            total.missed as usize,
            preset + appended.load(std::sync::atomic::Ordering::SeqCst)
        );
    }
}

/// Two readers rotating at once (two status polls in flight, or two app
/// instances on one config directory) while hooks keep missing. The record's
/// own contract is "an append in flight lands in the renamed inode, so no line
/// is lost"; a second reader that saw the old file as large and renames the
/// NEW, small current file over `.prev` destroys the whole first generation.
#[cfg(unix)]
#[test]
fn adv_w3c_broke_two_readers_rotating_at_once_lose_the_first_generation() {
    const TRIALS: usize = 400;
    const READERS: usize = 6;
    let mut lost = 0usize;
    let mut worst = (0usize, 0usize);
    for _ in 0..TRIALS {
        let dir = tempfile::tempdir().unwrap();
        let socket = socket_in(dir.path());
        let (text, preset) = big_record(NOW - HOUR);
        std::fs::write(record_path(&socket), text).unwrap();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let appended = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(READERS + 1));
        let writer = {
            let (socket, stop, appended, barrier) = (
                socket.clone(),
                stop.clone(),
                appended.clone(),
                barrier.clone(),
            );
            std::thread::spawn(move || {
                let record = DropRecord::with_clock(|| Some(NOW - 1));
                barrier.wait();
                // Stays well under one more rotation so any shortfall is the
                // race and not the two-generation size retention.
                while !stop.load(std::sync::atomic::Ordering::SeqCst)
                    && appended.load(std::sync::atomic::Ordering::SeqCst) < 100
                {
                    record
                        .append(&socket, "idle_prompt", Some("term-2-1"))
                        .expect("appended");
                    appended.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                }
            })
        };
        let readers: Vec<_> = (0..READERS)
            .map(|_| {
                let (socket, barrier) = (socket.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    read_record(&socket, NOW)
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
        stop.store(true, std::sync::atomic::Ordering::SeqCst);
        writer.join().unwrap();
        let expected = preset + appended.load(std::sync::atomic::Ordering::SeqCst);
        // The append total is under 100 lines (about 9 KiB): no honest second
        // rotation can have happened, so every line must still be in one of
        // the two generations.
        let seen = {
            let mut n = 0;
            for path in [record_path(&socket), prev_path(&record_path(&socket))] {
                if let Ok(text) = std::fs::read_to_string(&path) {
                    n += text.lines().filter(|l| !l.trim().is_empty()).count();
                }
            }
            n
        };
        if seen < expected {
            lost += 1;
            if expected - seen > worst.0 {
                worst = (expected - seen, expected);
            }
        }
    }
    assert_eq!(
        lost, 0,
        "{lost} of {TRIALS} trials lost lines to a double rotation (worst: {} of {})",
        worst.0, worst.1
    );
}

/// What one status poll costs with both generations at the largest size the
/// hook lets them reach (just under 64 KiB each). The panel polls on load and
/// on Recheck, so the bound that matters is "small", not "zero".
#[test]
fn adv_w3c_a_poll_over_two_full_generations_is_cheap() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let mut full = String::new();
    while full.len() < 63 * 1024 {
        full.push_str(&line(NOW - HOUR, "session_ended"));
    }
    // 63 KiB is over the 32 KiB rotation threshold, so each poll would rotate;
    // hold the shape by rewriting both generations before every timed read.
    let mut samples = Vec::new();
    for _ in 0..50 {
        std::fs::write(record_path(&socket), &full).unwrap();
        std::fs::write(prev_path(&record_path(&socket)), &full).unwrap();
        let started = std::time::Instant::now();
        let report = read_record(&socket, NOW);
        samples.push(started.elapsed());
        assert_eq!(report.error, None);
    }
    samples.sort();
    eprintln!(
        "adv_w3c poll over two ~63 KiB generations: min {:?} median {:?} max {:?}",
        samples[0],
        samples[samples.len() / 2],
        samples[samples.len() - 1]
    );
    assert!(samples[samples.len() / 2] < std::time::Duration::from_millis(50));
}

/// The reader is bounded in what it will take: one byte over the cap is an
/// error naming the cap, exactly at it is read.
#[test]
fn adv_w3c_the_read_cap_is_exact() {
    let dir = tempfile::tempdir().unwrap();
    let socket = socket_in(dir.path());
    let prev = prev_path(&record_path(&socket));
    std::fs::write(&prev, vec![b'x'; READ_CAP_BYTES as usize]).unwrap();
    let at = read_record(&socket, NOW);
    // Read, not refused. A generation this size is also past the hook's own
    // 64 KiB backstop, so since round 1 it carries the lower-bound note; what
    // it must not carry is the reader's own cap error.
    assert!(
        at.error
            .as_deref()
            .is_none_or(|error| !error.contains("KiB the reader takes")),
        "{:?}",
        at.error
    );
    std::fs::write(&prev, vec![b'x'; READ_CAP_BYTES as usize + 1]).unwrap();
    let over = read_record(&socket, NOW);
    assert!(over.error.is_some());
}
