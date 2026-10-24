//! The notification worker's sustained throughput: messages per second through
//! the intake (`alerts::try_offer`), the bounded queue and the real worker loop.
//!
//! Run with `cargo bench --manifest-path src-tauri/Cargo.toml --bench alerts_worker`.
//!
//! The subject is the production path, `alerts::start` and the worker thread
//! behind it, with a host that does nothing, so the figure is the worker's own
//! cost per notice (admit, coalescing state, standing, flush) and not the
//! notification centre's.
//!
//! # Method
//!
//! A producer offers a fixed number of notices as fast as the queue takes them,
//! retrying a full queue (counted in `queue_full_retries`, a metric). The queue
//! holds 64, so the time to hand over the last notice is the worker's time for
//! all but the last 64 of them: under 0.2% of a row's notices, and stated here
//! rather than hidden. The row's time is that span; `msgs_per_s` is the median
//! rate over the samples and the table's `min` column is the least-disturbed
//! sample, so the rate to compare between builds is `notices / min`, not the
//! median.
//!
//! # Rows
//!
//! * `hook state reports`: `turn_finished` and `tool_finished` alternating over
//!   64 sessions, the pair that races in the field. No banner, so the global
//!   rate limit is not what is measured.
//! * `renderer answers`: `prompt_submitted` over 64 sessions, the reserved
//!   class a typing user produces.
//!
//! Both are `time_gated: false`: a thread handing messages to another is as
//! much the scheduler's as the code's, so the baseline gate holds the counters
//! and the rate is read from the JSON beside the host-load snapshot.

#[cfg(unix)]
mod common;

#[cfg(unix)]
mod imp {
    use super::common::{interleave, Case, Report, Sample};
    use gitpulse_lib::alerts::{self, Host, Notice, Offer, Origin};
    use gitpulse_lib::tool_config::SessionAlertSettings;
    use std::sync::Arc;
    use std::time::Instant;

    const SESSIONS: usize = 64;
    const NOTICES: usize = 50_000;

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

    /// Offers `NOTICES` notices, each built by `make`, and times the span.
    fn run(make: impl Fn(usize) -> Notice) -> Sample {
        let mut retries = 0u64;
        let started = Instant::now();
        for n in 0..NOTICES {
            let mut pending = make(n);
            loop {
                // `try_offer` takes the notice; a refusal hands back nothing, so
                // a retry rebuilds it, which costs the producer and not the
                // worker.
                match alerts::try_offer(pending) {
                    Offer::Accepted | Offer::Coalesced => break,
                    Offer::QueueFull => {
                        retries += 1;
                        std::thread::yield_now();
                        pending = make(n);
                    }
                    other => panic!("the worker refused a notice: {other:?}"),
                }
            }
        }
        let elapsed = started.elapsed();
        Sample::default()
            .counter("notices", NOTICES as u64)
            .counter("sessions", SESSIONS as u64)
            .metric("msgs_per_s", NOTICES as f64 / elapsed.as_secs_f64())
            .metric("queue_full_retries", retries as f64)
            .elapsed(elapsed)
    }

    pub fn main() {
        let mut report = Report::new("alerts_worker");
        alerts::start(Arc::new(Idle));
        assert!(alerts::status().running, "the worker did not start");
        report.note(format!(
            "{NOTICES} notices per sample over {SESSIONS} sessions, retrying a full queue"
        ));
        let cases = vec![
            Case::new("hook state reports", || {
                run(|n| {
                    let event = if n % 2 == 0 {
                        "turn_finished"
                    } else {
                        "tool_finished"
                    };
                    notice(Origin::Hook, "hook", event, n)
                })
            })
            .ungated(),
            Case::new("renderer answers", || {
                run(|n| notice(Origin::Reader, "reader", "prompt_submitted", n))
            })
            .ungated(),
        ];
        let rows = interleave(cases, report.opts.samples(9), report.opts.warmup(1));
        report.extend(rows);
        report.finish();
    }
}

#[cfg(unix)]
fn main() {
    imp::main();
}

#[cfg(not(unix))]
fn main() {}
