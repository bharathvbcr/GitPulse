//! The notification worker's sustained throughput: messages per second through
//! the intake (`alerts::try_offer`), the 64-slot queue and the real worker loop.
//!
//! A measurement, not a gate: ignored by default, it prints and asserts only
//! that every notice was taken. Run it on a machine you are not using:
//!
//! ```text
//! cargo test --release --manifest-path src-tauri/Cargo.toml \
//!     --test alerts_worker_throughput -- --ignored --nocapture
//! ```
//!
//! `GITPULSE_BENCH_ROUNDS` (default 9) sets the samples per arm. The two arms
//! are interleaved within every round and their order rotates, so drift in
//! machine load lands on both; the figure to compare between builds is the
//! rate at the **minimum** time (the sample least disturbed by the machine).
//!
//! The subject is the production path with a host that does nothing, so the
//! number is the worker's own cost per notice and not the notification
//! centre's. A producer retries a full queue (counted and printed); the queue
//! holds 64, so the span is the worker's time for all but the last 64 notices
//! of a sample, under 0.2%.
//!
//! * `hook state reports`: `turn_finished` and `tool_finished` alternating over
//!   64 sessions, the pair that races in the field. No banner, so the global
//!   rate limit is not what is measured.
//! * `renderer answers`: `prompt_submitted` over 64 sessions, the reserved
//!   class a typing user produces.

#![cfg(unix)]

#[path = "../src/test_support/env.rs"]
mod hermetic_env;

use gitpulse_lib::alerts::{self, Host, Notice, Offer, Origin};
use gitpulse_lib::tool_config::SessionAlertSettings;
use std::sync::Arc;
use std::time::{Duration, Instant};

const SESSIONS: usize = 64;
const NOTICES: usize = 50_000;

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
fn run(make: impl Fn(usize) -> Notice) -> (Duration, u64) {
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

#[test]
#[ignore = "benchmark: run with --release --ignored"]
fn worker_msgs_per_second_min_of_n() {
    alerts::start(Arc::new(Idle));
    assert!(alerts::status().running, "the worker did not start");
    let rounds: usize = std::env::var("GITPULSE_BENCH_ROUNDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(9);
    let arms: [(&str, Make); 2] = [
        ("hook state reports", |n| {
            let event = if n % 2 == 0 {
                "turn_finished"
            } else {
                "tool_finished"
            };
            notice(Origin::Hook, "hook", event, n)
        }),
        ("renderer answers", |n| {
            notice(Origin::Reader, "reader", "prompt_submitted", n)
        }),
    ];
    // One untimed pass of each, then `rounds` timed ones, rotating the order.
    for (_, make) in &arms {
        run(make);
    }
    let mut spans: Vec<Vec<Duration>> = vec![Vec::new(); arms.len()];
    let mut retries: Vec<u64> = vec![0; arms.len()];
    for round in 0..rounds {
        for step in 0..arms.len() {
            let at = (round + step) % arms.len();
            let (span, full) = run(arms[at].1);
            spans[at].push(span);
            retries[at] += full;
        }
    }
    println!("\n{NOTICES} notices per sample over {SESSIONS} sessions, N = {rounds}");
    for (at, (name, _)) in arms.iter().enumerate() {
        spans[at].sort();
        let min = spans[at][0];
        let p50 = spans[at][spans[at].len() / 2];
        println!(
            "{name:<20} min {:>8.2} ms  p50 {:>8.2} ms  {:>10.0} msgs/s at min  ({} full-queue retries over {rounds} samples)",
            min.as_secs_f64() * 1e3,
            p50.as_secs_f64() * 1e3,
            NOTICES as f64 / min.as_secs_f64(),
            retries[at],
        );
        assert!(min > Duration::ZERO);
    }
}
