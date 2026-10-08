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
        event: None,
        detail: None,
        is_agent: true,
        channel,
        subject: None,
        subagent: false,
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
        std::thread::spawn(move || run(receiver, host, counters, last_error, Arc::default()))
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
        resolved: c.resolved.load(Ordering::Relaxed),
        displaced: c.displaced.load(Ordering::Relaxed),
        rate_limited: c.rate_limited.load(Ordering::Relaxed),
        deferred: c.deferred.load(Ordering::Relaxed),
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
        + status.resolved
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
        let _ = admit(
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

// ---- A fleet doing everything at once, in a random order -------------------
//
// The tests above load one dimension each. This one interleaves every kind of
// notice the worker can receive — hook banners, states, resolutions, bells,
// typed answers, plain shells, sessions outside GitPulse — across a fleet,
// with the clock jumping irregularly and the user looking at random tabs,
// and checks after every single step the invariants the rest of this module
// promises piecemeal. Seeded, so a failure names a step that replays.

/// xorshift64*: deterministic, dependency-free, good enough to shuffle.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: u64) -> u64 {
        self.next() % n
    }
}

#[derive(Default)]
struct Observer {
    attended: Mutex<Option<String>>,
    /// Banners currently believed to be in the notification centre.
    shown: Mutex<std::collections::HashSet<String>>,
    /// What a renderer that applied every announcement would show.
    view: Mutex<HashMap<String, &'static str>>,
    /// Sessions announced since the harness last looked.
    announced: Mutex<std::collections::HashSet<String>>,
    spurious_withdrawals: Mutex<Vec<String>>,
}

impl Host for Observer {
    fn submit(&self, native: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        self.shown.lock().unwrap().insert(native.to_owned());
        Ok(true)
    }
    fn attended(&self, key: &str) -> bool {
        self.attended.lock().unwrap().as_deref() == Some(key)
    }
    fn config(&self) -> SessionAlertSettings {
        SessionAlertSettings {
            enabled: true,
            ..SessionAlertSettings::default()
        }
    }
    fn minute(&self) -> Option<u16> {
        Some(12 * 60)
    }
    fn announce(&self, attention: &Attention) {
        self.announced
            .lock()
            .unwrap()
            .insert(attention.session.clone());
        let mut view = self.view.lock().unwrap();
        let clears = attention
            .event
            .and_then(bridge::event)
            .is_some_and(|event| event.need == bridge::Need::Clear);
        if clears {
            view.remove(&attention.session);
        } else {
            view.insert(
                attention.session.clone(),
                attention.event.unwrap_or("signal"),
            );
        }
    }
    fn withdraw(&self, native: &str) {
        if !self.shown.lock().unwrap().remove(native) {
            self.spurious_withdrawals
                .lock()
                .unwrap()
                .push(native.to_owned());
        }
    }
}

fn random_notice(rng: &mut Rng, sessions: u64) -> Notice {
    let index = rng.below(sessions);
    // A few keys the renderer can never show: a plain shell and a Claude
    // Code outside any GitPulse terminal.
    let (key, is_agent) = match index {
        0 => ("term-shell".to_owned(), false),
        1 => ("hook-claude-outside".to_owned(), true),
        n => (format!("term-{n}"), true),
    };
    let subject = format!("{:x}", rng.below(4));
    let mut notice = match rng.below(10) {
        0..=2 => super::stress::notice(&key, "bell"),
        3 => Notice {
            origin: Origin::Reader,
            channel: "reader",
            event: Some("prompt_submitted"),
            ..super::stress::notice(&key, "reader")
        },
        _ => {
            let event = bridge::EVENTS[rng.below(bridge::EVENTS.len() as u64) as usize];
            Notice {
                origin: Origin::Hook,
                channel: "hook",
                event: Some(event.name),
                detail: Some(format!("detail {}", rng.below(100))),
                subject: matches!(event.name, "permission_request" | "tool_finished")
                    .then_some(subject),
                subagent: rng.below(4) == 0,
                ..super::stress::notice(&key, "hook")
            }
        }
    };
    notice.key = key;
    notice.is_agent = is_agent;
    notice
}

#[test]
fn a_fleet_doing_everything_at_once_keeps_every_promise() {
    // Two fleet sizes: one the tracker holds whole, and one past its bound,
    // so eviction — and what it costs — is exercised as well.
    let past_the_bound = MAX_TRACKED as u64 * 2;
    for (seed, sessions) in [
        (1u64, 24),
        (7, 24),
        (42, 24),
        (1_000_003, past_the_bound),
        (0xdead_beef, past_the_bound),
    ] {
        let mut rng = Rng(seed);
        let observer = Arc::new(Observer::default());
        let host: Arc<dyn Host> = observer.clone();
        let counters = Counters::default();
        let last_error = Mutex::new(None);
        let mut tracked: HashMap<String, Tracked> = HashMap::new();
        let start = Instant::now();
        let mut bucket = Bucket::new(start);
        let mut now = start;
        let mut banner_offers = 0u64;
        let mut amnesic = std::collections::HashSet::new();
        for step in 0..60_000u32 {
            // Irregular time: bursts inside one window, gaps across several.
            // Past the bound, a storm: everything inside a few windows, so
            // sessions are evicted while banners are still held.
            now += Duration::from_millis(if sessions > MAX_TRACKED as u64 {
                rng.below(20)
            } else {
                match rng.below(10) {
                    0..=5 => rng.below(200),
                    6..=8 => rng.below(3_000),
                    _ => rng.below(120_000),
                }
            });
            if rng.below(20) == 0 {
                *observer.attended.lock().unwrap() =
                    (rng.below(3) == 0).then(|| format!("term-{}", rng.below(sessions)));
            }
            let notice = random_notice(&mut rng, sessions);
            if notice.role() == bridge::Role::Banner {
                banner_offers += 1;
            }
            let standing_before: std::collections::HashSet<String> = tracked
                .iter()
                .filter(|(_, e)| e.standing.is_some())
                .map(|(k, _)| k.clone())
                .collect();
            if let Some(last) = admit(notice, now, &mut tracked, &counters) {
                host.announce(&last);
            }
            flush(
                now,
                false,
                &mut tracked,
                &mut bucket,
                &host,
                &counters,
                &last_error,
            );
            prune(now, &mut tracked);
            // A session evicted while it stood asking leaves the pane holding
            // what the worker forgot. Until the worker next speaks about it,
            // the two may differ — that is what eviction costs, and why it
            // takes idle sessions first. Pruning never does this: it keeps
            // every session a hook says is asking.
            for key in standing_before {
                if !tracked.contains_key(&key) {
                    amnesic.insert(key);
                }
            }
            for key in observer.announced.lock().unwrap().drain() {
                amnesic.remove(&key);
            }

            let at = |what: &str| format!("seed {seed} step {step}: {what}");
            assert!(
                tracked.len() <= MAX_TRACKED,
                "{}",
                at("the map outgrew its bound")
            );
            // Liveness: anything held has a time it will go out, so the
            // worker can never sleep on work.
            for (key, entry) in &tracked {
                if entry.pending.is_some() {
                    assert!(
                        entry.due_at().is_some(),
                        "{}",
                        at(&format!("{key} held with no due time"))
                    );
                }
                if let Some(standing) = &entry.standing {
                    assert!(
                        !key.starts_with("hook-") && key != "term-shell",
                        "{}",
                        at(&format!(
                            "{key} stands asking, but no pane can show it: {:?}",
                            standing.attention
                        ))
                    );
                }
            }
            if tracked.values().any(Tracked::holds_work) {
                assert!(
                    next_wake(&tracked).is_some(),
                    "{}",
                    at("work held and nothing to wake for")
                );
            }
            // Every banner offered is in exactly one outcome or still held.
            let held = tracked.values().filter(|e| e.pending.is_some()).count() as u64;
            let c = &counters;
            let outcomes = c.delivered.load(Ordering::Relaxed)
                + c.suppressed_disabled.load(Ordering::Relaxed)
                + c.suppressed_not_agent.load(Ordering::Relaxed)
                + c.suppressed_attended.load(Ordering::Relaxed)
                + c.suppressed_quiet.load(Ordering::Relaxed)
                + c.coalesced.load(Ordering::Relaxed)
                + c.resolved.load(Ordering::Relaxed)
                + c.displaced.load(Ordering::Relaxed)
                + c.rate_limited.load(Ordering::Relaxed)
                + c.failed.load(Ordering::Relaxed);
            assert_eq!(
                outcomes + held,
                banner_offers,
                "{}",
                at("a banner went unaccounted for")
            );
            // What a renderer would show agrees with what the worker holds,
            // for every session with nothing still waiting to be said. (An
            // evicted session leaves the map, and with it this comparison.)
            let view = observer.view.lock().unwrap();
            for (key, entry) in &tracked {
                if entry.unannounced.is_some() || amnesic.contains(key) {
                    continue;
                }
                let worker = entry
                    .standing
                    .as_ref()
                    .map(|s| s.attention.event.unwrap_or("signal"));
                assert_eq!(
                    view.get(key).copied(),
                    worker,
                    "{}",
                    at(&format!("{key}: pane and worker disagree"))
                );
            }
            drop(view);
            assert!(
                observer.spurious_withdrawals.lock().unwrap().is_empty(),
                "{}",
                at("withdrew a banner that was never shown")
            );
        }
        // Drain: everything held goes out or is counted, nothing is lost.
        flush(
            now + Duration::from_secs(3_600),
            true,
            &mut tracked,
            &mut bucket,
            &host,
            &counters,
            &last_error,
        );
        assert!(
            tracked.values().all(|e| e.pending.is_none()),
            "seed {seed}: held after the final pass"
        );
        assert!(
            counters.delivered.load(Ordering::Relaxed) > 100,
            "seed {seed}: the run delivered too little to have exercised anything"
        );
        if sessions > MAX_TRACKED as u64 {
            assert!(
                counters.displaced.load(Ordering::Relaxed) > 0,
                "seed {seed}: a fleet past the bound displaced nothing, so eviction was not exercised"
            );
        }
    }
}
