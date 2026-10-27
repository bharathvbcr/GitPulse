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
    let counters = Arc::new(Counters::default());
    let (intake, receiver) = Intake::channel(counters.clone());
    let bells = intake.bells();
    let intake = Arc::new(intake);
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
    let worker = {
        let counters = counters.clone();
        let last_error = last_error.clone();
        std::thread::spawn(move || run(receiver, bells, host, counters, last_error, Arc::default()))
    };

    let work: Vec<Vec<Notice>> = offered
        .chunks(offered.len().div_ceil(threads.max(1)))
        .map(<[Notice]>::to_vec)
        .collect();
    let mut handles = Vec::new();
    for chunk in work {
        let intake = intake.clone();
        handles.push(std::thread::spawn(move || {
            for notice in chunk {
                // Exactly what `offer` does, including its refusal to block.
                intake.offer(notice);
            }
        }));
    }
    for handle in handles {
        handle.join().unwrap();
    }
    // Closing the channel ends the loop once it has drained what it holds.
    drop(intake);
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
        dropped_queue: c.dropped_queue.load(Ordering::Relaxed),
        dropped_scan: 0,
        failed: c.failed.load(Ordering::Relaxed),
        bridge_rejected: 0,
        stale: c.stale.load(Ordering::Relaxed),
        hook_replies_missed: 0,
        hook_reply_missed_last: None,
        hook_drop_record_error: None,
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
        + status.stale
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
        started.elapsed() < crate::test_support::coverage_relaxed(Duration::from_secs(30)),
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
        elapsed < crate::test_support::coverage_relaxed(Duration::from_secs(20)),
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

// ---- The same fleet, with its hook reports arriving out of order ------------
//
// The fleet above feeds the worker's decisions directly. This one goes in at
// the door every hook report uses (`Intake::offer_stamped`) with each report
// carrying the stamp its hook took, delivered in a shuffled order: neighbours
// swapped, and a few held back a long way. An independent model of the order
// rule says which reports are stale; the intake must agree on every one, and
// the totals must still reconcile with `accounted()`.

#[cfg(unix)]
#[test]
fn a_fleet_whose_reports_arrive_out_of_order_still_accounts_for_every_one() {
    for seed in [3u64, 11, 0x5eed] {
        let mut rng = Rng(seed);
        let sessions = 24u64;
        let total = 6_000usize;
        // What happened, in the order it happened: (stamp, session, event).
        let happened: Vec<(u64, String, &'static bridge::Event)> = (0..total)
            .map(|at| {
                let session = format!("term-{}", 2 + rng.below(sessions));
                let event = &bridge::EVENTS[rng.below(bridge::EVENTS.len() as u64) as usize];
                (at as u64 + 1, session, event)
            })
            .collect();
        // What arrived, in the order it arrived.
        let mut arrival: Vec<usize> = (0..total).collect();
        for at in 0..total - 1 {
            if rng.below(4) == 0 {
                arrival.swap(at, at + 1);
            }
        }
        for at in (0..total - 24).step_by(37) {
            arrival.swap(at, at + 1 + rng.below(23) as usize);
        }
        assert_ne!(
            arrival,
            (0..total).collect::<Vec<_>>(),
            "the shuffle left everything in order"
        );

        let counters = Arc::new(Counters::default());
        let (intake, receiver) = Intake::channel(counters.clone());
        let bells = intake.bells();
        let host: Arc<dyn Host> = Arc::new(Sink {
            delivered: Mutex::new(Vec::new()),
            config: SessionAlertSettings {
                enabled: true,
                ..SessionAlertSettings::default()
            },
        });
        let worker = {
            let (counters, host) = (counters.clone(), host.clone());
            std::thread::spawn(move || {
                run(
                    receiver,
                    bells,
                    host,
                    counters,
                    Arc::default(),
                    Arc::default(),
                )
            })
        };
        let mut banners = 0u64;
        let mut non_banners_lost = 0u64;
        // The order rule, stated again from its description rather than copied
        // from `OrderBook`, and judged as the reports arrive because whether a
        // report is remembered as a source of twins depends on whether the
        // queue took it: a report older than the newest for its session is
        // stale; one older than the session's end is stale; a session that
        // ended and is heard from again after its end is a new run. A report
        // that is not stale and is the same event as one the queue took for
        // its session within the window (the last eight) is a twin: folded, and
        // moving nothing.
        let mut newest: HashMap<String, u64> = HashMap::new();
        let mut ended: HashMap<String, u64> = HashMap::new();
        let mut taken: HashMap<String, Vec<(u64, &'static str)>> = HashMap::new();
        let (mut model_stale, mut model_twins) = (0u64, 0u64);
        for &index in &arrival {
            let (stamp, session, event) = &happened[index];
            let behind_end = ended.get(session).is_some_and(|end| stamp < end);
            let behind_newest = newest.get(session).is_some_and(|seen| stamp < seen);
            let twin = !(behind_end || behind_newest)
                && taken.get(session).is_some_and(|queued| {
                    queued.iter().any(|(at, name)| {
                        *name == event.name
                            && at.abs_diff(*stamp) <= super::intake::DUPLICATE_WINDOW_NS
                    })
                });
            let json = format!(
                r#"{{"v":1,"event":"{}","agent":"claude","session":"{session}"}}"#,
                event.name
            );
            let (notice, _) = bridge::parse_report_stamped(json.as_bytes()).unwrap();
            let offer = intake.offer_stamped(notice, *stamp);
            if behind_end || behind_newest {
                model_stale += 1;
                assert_eq!(
                    offer,
                    Offer::Stale,
                    "seed {seed}: report {index} ({} for {session} at {stamp}) disagrees with the model",
                    event.name
                );
            } else if twin {
                model_twins += 1;
                assert_eq!(
                    offer,
                    Offer::Coalesced,
                    "seed {seed}: report {index} ({} for {session} at {stamp}) is a twin",
                    event.name
                );
            } else {
                assert!(
                    matches!(offer, Offer::Accepted | Offer::QueueFull),
                    "seed {seed}: report {index} ({} for {session} at {stamp}): {offer:?}",
                    event.name
                );
                ended.remove(session);
                newest.insert(session.clone(), *stamp);
                if event.name == "session_ended" {
                    ended.insert(session.clone(), newest.remove(session).unwrap_or(*stamp));
                }
                if offer == Offer::Accepted {
                    let queued = taken.entry(session.clone()).or_default();
                    if queued.len() >= super::intake::RECENT_PER_SESSION {
                        queued.remove(0);
                    }
                    queued.push((*stamp, event.name));
                }
            }
            if event.role == bridge::Role::Banner {
                banners += 1;
            } else if !offer.taken() {
                non_banners_lost += 1;
            }
        }
        drop(intake);
        worker.join().unwrap();
        let c = &counters;
        let status = SessionAlertStatus {
            delivered: c.delivered.load(Ordering::Relaxed),
            suppressed_disabled: c.suppressed_disabled.load(Ordering::Relaxed),
            suppressed_not_agent: c.suppressed_not_agent.load(Ordering::Relaxed),
            suppressed_attended: c.suppressed_attended.load(Ordering::Relaxed),
            suppressed_quiet: c.suppressed_quiet.load(Ordering::Relaxed),
            coalesced: c.coalesced.load(Ordering::Relaxed),
            resolved: c.resolved.load(Ordering::Relaxed),
            displaced: c.displaced.load(Ordering::Relaxed),
            rate_limited: c.rate_limited.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
            dropped_queue: c.dropped_queue.load(Ordering::Relaxed),
            stale: c.stale.load(Ordering::Relaxed),
            ..SessionAlertStatus::default()
        };
        assert_eq!(status.stale, model_stale, "seed {seed}");
        assert!(
            model_stale > total as u64 / 200,
            "seed {seed}: only {model_stale} reports were stale, so ordering was barely exercised"
        );
        assert!(
            model_twins > 0,
            "seed {seed}: no twin was offered, so folding was not exercised"
        );
        // A banner ends in exactly one outcome, whatever became of it; a state
        // or a resolution has no outcome of its own, so only losing one shows.
        assert_eq!(
            accounted(&status),
            banners + non_banners_lost,
            "seed {seed}: a report went unaccounted for: {status:?}"
        );
    }
}
