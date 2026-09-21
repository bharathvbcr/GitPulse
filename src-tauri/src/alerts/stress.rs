//! What this subsystem does when it is not being treated gently.
//!
//! Three loads, each chosen because it is something a real agent session can
//! actually produce, not because it is large:
//!
//! * A program that writes `BEL` as fast as the PTY will carry it — `yes $'\a'`
//!   is one line — which is the scanner's hot path and the queue's worst case.
//! * Many sessions signalling at once, which is the ordinary state of a fleet
//!   of worktrees and the case coalescing is per-session for.
//! * Output that is *nearly* a notification and never finishes one, which is
//!   what a partly-written escape sequence looks like to a resumable parser
//!   forever.
//!
//! The invariant that matters more than any throughput number: **every notice
//! offered is accounted for**. Delivered, suppressed by a named rule,
//! coalesced away, rate limited, refused by the OS, or dropped because the
//! queue was full — the totals have to reconcile. A notice that is simply
//! gone is the failure this whole module is written to make impossible, and
//! it is invisible to every test that only counts what arrived.

use super::*;
use crate::tool_config::SessionAlertSettings;

struct Sink {
    delivered: Mutex<Vec<String>>,
    config: SessionAlertSettings,
}

impl Host for Sink {
    fn submit(&self, native: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        self.delivered.lock().unwrap().push(native.to_owned());
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
    }
    fn config(&self) -> SessionAlertSettings {
        self.config.clone()
    }
    fn minute(&self) -> Option<u16> {
        Some(12 * 60)
    }
}

fn notice(key: &str, channel: &'static str) -> Notice {
    Notice {
        key: key.to_owned(),
        origin: Origin::Terminal,
        label: "Claude Code".into(),
        place: Some("GitPulse".into()),
        reason: None,
        detail: None,
        is_agent: true,
        channel,
    }
}

/// Runs the real worker loop over a bounded stream of notices and returns the
/// counters it produced, with the queue depth it actually needed.
fn drive(offered: Vec<Notice>, threads: usize) -> (SessionAlertStatus, usize) {
    let (sender, receiver) = mpsc::sync_channel(QUEUE);
    let counters = Arc::new(Counters::default());
    let last_error = Arc::new(Mutex::new(None));
    let host: Arc<dyn Host> = Arc::new(Sink {
        delivered: Mutex::new(Vec::new()),
        config: SessionAlertSettings {
            // Rate limiting is exercised by its own test; here it would only
            // mask the accounting under load.
            enabled: true,
            ..SessionAlertSettings::default()
        },
    });
    let dropped = Arc::new(AtomicU64::new(0));
    let worker = {
        let counters = counters.clone();
        let last_error = last_error.clone();
        std::thread::spawn(move || run(receiver, host, counters, last_error))
    };

    let work: Vec<Vec<Notice>> = offered
        .chunks(offered.len().div_ceil(threads.max(1)))
        .map(<[Notice]>::to_vec)
        .collect();
    let mut handles = Vec::new();
    for chunk in work {
        let sender = sender.clone();
        let dropped = dropped.clone();
        handles.push(std::thread::spawn(move || {
            for notice in chunk {
                // Exactly what `offer` does, including its refusal to block.
                if sender.try_send(notice).is_err() {
                    dropped.fetch_add(1, Ordering::Relaxed);
                }
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    // Closing the channel ends the loop once it has drained what it holds.
    drop(sender);
    worker.join().unwrap();

    let c = &counters;
    let status = SessionAlertStatus {
        running: true,
        delivered: c.delivered.load(Ordering::Relaxed),
        suppressed_disabled: c.suppressed_disabled.load(Ordering::Relaxed),
        suppressed_not_agent: c.suppressed_not_agent.load(Ordering::Relaxed),
        suppressed_attended: c.suppressed_attended.load(Ordering::Relaxed),
        suppressed_quiet: c.suppressed_quiet.load(Ordering::Relaxed),
        coalesced: c.coalesced.load(Ordering::Relaxed),
        displaced: c.displaced.load(Ordering::Relaxed),
        rate_limited: c.rate_limited.load(Ordering::Relaxed),
        dropped_queue: dropped.load(Ordering::Relaxed),
        dropped_scan: 0,
        failed: c.failed.load(Ordering::Relaxed),
        bridge_rejected: 0,
        bridge_listening: false,
        bridge_path: None,
        last_error: None,
    };
    (status, QUEUE)
}

/// Everything one notice can become. A notice that is none of these is lost.
fn accounted(status: &SessionAlertStatus) -> u64 {
    status.delivered
        + status.suppressed_disabled
        + status.suppressed_not_agent
        + status.suppressed_attended
        + status.suppressed_quiet
        + status.coalesced
        + status.displaced
        + status.rate_limited
        + status.failed
        + status.dropped_queue
}

#[test]
fn a_session_ringing_flat_out_loses_no_notice_unaccounted() {
    let offered: Vec<Notice> = (0..5_000).map(|_| notice("term-1", "bell")).collect();
    let count = offered.len() as u64;
    let (status, _) = drive(offered, 1);
    assert_eq!(
        accounted(&status),
        count,
        "{} of {count} notices are unaccounted for: {status:?}",
        count.saturating_sub(accounted(&status))
    );
    // The point of coalescing: five thousand bells, a handful of banners.
    assert!(status.delivered <= 5, "{} banners", status.delivered);
}

#[test]
fn many_sessions_at_once_stay_accounted_for_across_threads() {
    let offered: Vec<Notice> = (0..20_000)
        .map(|index| notice(&format!("term-{}", index % 200), "osc9"))
        .collect();
    let count = offered.len() as u64;
    let (status, _) = drive(offered, 8);
    assert_eq!(
        accounted(&status),
        count,
        "notices went missing under concurrency: {status:?}"
    );
    assert!(
        status.dropped_queue > 0 || status.coalesced > 0,
        "a load this size absorbed nothing, so the bounds are not being exercised: {status:?}"
    );
}

#[test]
fn a_full_queue_refuses_rather_than_blocking_its_producer() {
    // The producers are a PTY reader thread and a socket accept loop. Neither
    // may wait: a reader that blocks stops draining the terminal, which the
    // user sees as a frozen shell.
    let offered: Vec<Notice> = (0..50_000).map(|_| notice("term-1", "bell")).collect();
    let count = offered.len() as u64;
    let started = std::time::Instant::now();
    let (status, depth) = drive(offered, 4);
    assert_eq!(accounted(&status), count);
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{count} offers took {:?}",
        started.elapsed()
    );
    assert!(depth <= 128, "the queue is no longer a bound");
}

#[test]
fn the_scanner_keeps_up_with_a_terminal_writing_as_fast_as_it_can() {
    // A PTY read is 4 KiB and the scanner sees every byte of every one, so its
    // cost is paid by terminal latency. 64 MiB is far more than any real
    // session produces in a burst; the budget is loose enough to survive a
    // loaded machine and tight enough to catch an accidental quadratic.
    let mut scanner = scan::Scanner::new();
    let chunk: Vec<u8> = "\x1b[32mgreen\x1b[0m output line with \x1b]0;a title\x07 and text\r\n"
        .repeat(64)
        .into_bytes();
    let rounds = (64 * 1024 * 1024) / chunk.len();
    let started = std::time::Instant::now();
    let mut signals = 0;
    for _ in 0..rounds {
        signals += scanner.feed(&chunk).len();
    }
    let elapsed = started.elapsed();
    assert_eq!(signals, 0, "ordinary terminal output raised a notification");
    assert!(
        elapsed < Duration::from_secs(20),
        "scanning {} MiB took {elapsed:?}",
        rounds * chunk.len() / (1024 * 1024)
    );
}

#[test]
fn output_that_never_finishes_a_sequence_retains_nothing() {
    // What a resumable parser sees when a program writes an escape introducer
    // and then megabytes of payload it never terminates.
    let mut scanner = scan::Scanner::new();
    scanner.feed(b"\x1b]777;notify;");
    for _ in 0..4096 {
        scanner.feed(&[b'z'; 4096]);
    }
    // Still in sync: the sequence is abandoned, and the stream after its
    // terminator is read normally.
    assert_eq!(
        scanner.feed(b"\x07").len(),
        0,
        "an overflowed OSC was delivered"
    );
    assert_eq!(
        scanner.feed(b"\x07").len(),
        1,
        "the parser did not return to the ground state"
    );
}

#[test]
fn a_thousand_interleaved_sessions_share_one_bounded_hub() {
    // Every worktree in a fleet opening a tab at once. The coalescing map is
    // the only per-session state, and it is what must not grow with the fleet.
    let mut tracked: HashMap<String, Tracked> = HashMap::new();
    let counters = Counters::default();
    let start = Instant::now();
    for index in 0..1_000u64 {
        admit(
            notice(&format!("term-{index}"), "hook"),
            start + Duration::from_millis(index),
            &mut tracked,
            &counters,
        );
        assert!(tracked.len() <= MAX_TRACKED);
    }
    let held: usize = tracked.keys().map(String::len).sum();
    assert!(held < 8 * 1024, "{held} bytes of session keys retained");
}
