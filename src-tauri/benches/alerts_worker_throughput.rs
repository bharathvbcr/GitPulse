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
//! Both rows are `ungated`: their time is a hand-off between a producer that
//! yields on a full queue and a worker thread, which is a property of the
//! scheduler and the core count of the runner, not of the code. Their counters
//! (notices and sessions) stay in the gate.

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

fn arm(name: &'static str, make: Make) -> Case<'static> {
    Case::new(name, move || {
        let (span, retries) = run(make);
        assert!(span > Duration::ZERO, "{name}: a sample took no time");
        Sample::default()
            .counter("notices", NOTICES as u64)
            .counter("sessions", SESSIONS as u64)
            .metric("full_queue_retries", retries as f64)
            .metric("msgs_per_s", NOTICES as f64 / span.as_secs_f64())
            .elapsed(span)
    })
    .ungated()
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
