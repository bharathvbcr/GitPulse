//! The notification worker's sustained throughput: messages per second through
//! the intake (`alerts::try_offer`), the 64-slot queue and the real worker loop.
//!
//! Run with
//! `cargo bench --manifest-path src-tauri/Cargo.toml --bench alerts_worker_throughput`,
//! on a machine you are not using. It started as an ignored integration test
//! because `Cargo.toml` was held when it was written; it is a registered bench
//! on the shared harness now, so `benches.yml` runs it and a work counter that
//! changes fails the compare.
//!
//! The two arms are interleaved within every round and their order rotates, so
//! drift in machine load lands on both; the figure to compare between builds is
//! the rate at the **minimum** time (the sample least disturbed by the machine),
//! which is `notices / min`.
//!
//! The subject is the production path with a host that does nothing, so the
//! number is the worker's own cost per notice and not the notification
//! centre's. A producer retries a full queue (counted, and reported as a
//! metric: it is a race with the worker, so nothing gates on it); the queue
//! holds 64, so the span is the worker's time for all but the last 64 notices
//! of a sample, under 0.2%.
//!
//! * `hook state reports`: `turn_finished` and `tool_finished` alternating over
//!   64 sessions, the pair that races in the field. No banner, so the global
//!   rate limit is not what is measured.
//! * `renderer answers`: `prompt_submitted` over 64 sessions, the reserved
//!   class a typing user produces.
//!
//! Both rows are gated on time and on counters, like the other benches: the
//! hand-off between the producer and the worker thread is the work being
//! measured, not a property of the host. A producer that gets slower, or a
//! worker that stops taking notices, fails the compare. The counters are
//! `notices` and `sessions` (inputs) and `drained`, which is measured: 1 when
//! the worker took a probe offered after the sample's last notice (the queue is
//! one FIFO, so it took them all), 0 when it did not within 30 s. A worker that
//! stalls or dies makes it 0.

mod common;

use common::{interleave, Case, Report, Sample};
use gitpulse_lib::alerts::{self, Host, Notice, Offer, Origin};
use gitpulse_lib::tool_config::SessionAlertSettings;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SESSIONS: usize = 64;
const NOTICES: usize = 50_000;
/// Timed samples per arm in a full run (`--smoke` cuts it).
const ROUNDS: usize = 9;

/// How an arm builds its nth notice.
type Make = fn(usize) -> Notice;

struct Idle;

impl Host for Idle {
    fn submit(&self, _: &str, _: &str, _: &str, _: bool) -> Result<bool, String> {
        Ok(true)
    }
    fn attended(&self, _: &str) -> bool {
        false
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
}

fn notice(origin: Origin, channel: &'static str, event: &'static str, n: usize) -> Notice {
    Notice {
        key: format!("term-bench-{}", n % SESSIONS),
        origin,
        label: "Claude Code".into(),
        place: None,
        event: Some(event),
        detail: None,
        is_agent: true,
        channel,
        subject: None,
        subagent: false,
    }
}

/// Offers `NOTICES` notices built by `make`; returns the span and the number
/// of times the queue was full.
fn run(make: Make) -> (Duration, u64) {
    let mut retries = 0u64;
    let started = Instant::now();
    for n in 0..NOTICES {
        loop {
            match alerts::try_offer(make(n)) {
                Offer::Accepted | Offer::Coalesced => break,
                Offer::QueueFull => {
                    retries += 1;
                    std::thread::yield_now();
                }
                other => panic!("the worker refused a notice: {other:?}"),
            }
        }
    }
    (started.elapsed(), retries)
}

/// How long the worker is given to take everything a sample offered, once the
/// last notice is in. Outside the timed span; a worker that never catches up
/// reads as `drained = 0`, which fails the compare.
const DRAIN: Duration = Duration::from_secs(30);

/// Offers a probe through the queue every notice of the sample went through and
/// waits for the worker to stand it; 1 when it did within [`DRAIN`], else 0.
///
/// The queue is one FIFO, so a worker that has taken the probe has taken every
/// notice offered before it. A hook `permission_request` for a session of its
/// own is what the worker records as a standing question, which
/// `alerts::standing` reports. The probe is answered afterwards so nothing
/// stands from one sample to the next.
fn drained(sample: usize) -> u64 {
    let key = format!("term-bench-probe-{sample}");
    let mut probe = notice(Origin::Hook, "hook", "permission_request", 0);
    probe.key.clone_from(&key);
    let started = Instant::now();
    while !alerts::try_offer(probe.clone()).taken() {
        if started.elapsed() > DRAIN {
            return 0;
        }
        std::thread::yield_now();
    }
    let stood = loop {
        if alerts::standing()
            .iter()
            .any(|asking| asking.session == key)
        {
            break 1;
        }
        if started.elapsed() > DRAIN {
            break 0;
        }
        std::thread::sleep(Duration::from_millis(1));
    };
    let _ = alerts::answer(&key);
    stood
}

fn arm(name: &'static str, make: Make) -> Case<'static> {
    let probes = std::sync::atomic::AtomicUsize::new(0);
    Case::new(name, move || {
        let (span, retries) = run(make);
        assert!(span > Duration::ZERO, "{name}: a sample took no time");
        let drained = drained(probes.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
        Sample::default()
            .counter("notices", NOTICES as u64)
            .counter("sessions", SESSIONS as u64)
            .counter("drained", drained)
            .metric("full_queue_retries", retries as f64)
            .metric("msgs_per_s", NOTICES as f64 / span.as_secs_f64())
            .elapsed(span)
    })
}

fn main() {
    alerts::start(Arc::new(Idle));
    assert!(alerts::status().running, "the worker did not start");
    let mut report = Report::new("alerts_worker_throughput");
    report.note(format!(
        "{NOTICES} notices per sample over {SESSIONS} sessions through alerts::try_offer, \
         the 64-slot queue and the real worker, a full queue retried"
    ));
    let rows = interleave(
        vec![
            arm("hook state reports", |n| {
                let event = if n % 2 == 0 {
                    "turn_finished"
                } else {
                    "tool_finished"
                };
                notice(Origin::Hook, "hook", event, n)
            }),
            arm("renderer answers", |n| {
                notice(Origin::Reader, "reader", "prompt_submitted", n)
            }),
        ],
        report.opts.samples(ROUNDS),
        report.opts.warmup(1),
    );
    report.extend(rows);
    report.finish();
}
