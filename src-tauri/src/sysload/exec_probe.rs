//! How long it takes this machine to start a process, measured by starting one.
//!
//! # Why
//!
//! On 2026-11-16 `/usr/bin/true` took a median 1.36 s to exec (n=12) and
//! `git rev-parse HEAD` 1.1 to 1.8 s outside any gate, with `syspolicyd` at
//! 445% CPU, swap at 12.5 of 13.3 GB and a load of 22 to 26 on 18 cores. Every
//! view that asks git one process per question became multi-second, and the
//! spawn gate's token wait was a minor share of it. Nothing the sampler read
//! (memory level, swap, load, disk) names that failure on its own, so the
//! governor could not shed the optional work that was queueing behind it. This
//! module measures the thing itself: the wall time of a child that does
//! nothing.
//!
//! # What is measured
//!
//! From the moment the spawn gate admits the child to the moment the runner has
//! reaped it (`engine::process::time_exec_probe`). The wait for a slot or a
//! token is the gate's own latency and is excluded, so the number is the
//! machine's. The child runs at standard QoS, like the reactive `git` reads the
//! signal protects: a background-band child is throttled by design and would
//! read slower than the work it speaks for. The runner's reap loop adds a few
//! milliseconds; against a threshold in hundreds of milliseconds that is noise.
//!
//! # What a probe that did not give a number says
//!
//! * It ran and exited: a sample.
//! * It hit its own deadline: a sample at the deadline. That is the slowest
//!   outcome there is, and reading it as "no signal" would blind the governor
//!   exactly when start-up is worst. The value is a lower bound.
//! * The gate refused it, no slot came free, it failed to spawn or it exited
//!   non-zero: not a sample, and recorded as a failed probe.
//!
//! A window that has too few samples, whose newest outcome is old, or whose
//! latest probes all failed reads as unavailable, never as fast
//! ([`Window::reading`]).
//!
//! # Hysteresis
//!
//! The governor counts 2 s sensor ticks (three to enter a tier, five to leave
//! it), but this reading changes once per probe, so counting ticks alone would
//! let the verdict follow the probe cadence. [`Window::push`] therefore latches
//! `slow` on the median: set at or above [`ENTER_MS`], cleared only below
//! [`EXIT_MS`], unchanged between. The latch moves only when a probe lands, so
//! re-reading the same window never changes it.
//!
//! # Cadence and cost
//!
//! One probe every [`CADENCE`] (15 s), never more than one in flight (one
//! thread, and a probe is bounded by [`DEADLINE`]). That is four short-lived
//! children a minute. The probe is `Background` class and bypasses both the
//! rate bucket and the pressure verdict (`with_exec_probe`): the rate bucket is
//! the git budget it must not spend or be refused by, and a probe that the
//! slow-start verdict could shed would never see start-up recover. It is still
//! bounded by the concurrency limit, its deadline and its own cadence.

use super::sensor::{Reading, Why};
use serde::Serialize;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A median at or above this many milliseconds is slow start-up. The median of
/// healthy macOS hosts for a `true` child is single-digit milliseconds, and the
/// 2026-11-16 incident read 1,360; 500 ms is where every `git` call in a view
/// is already half a second late. Not measured as an optimum.
pub const ENTER_MS: u64 = 500;
/// The median must fall below this before start-up counts as recovered: 60% of
/// [`ENTER_MS`], so a median hovering at the line does not flap.
pub const EXIT_MS: u64 = 300;
/// Time between the starts of two probes.
pub const CADENCE: Duration = Duration::from_secs(15);
/// A probe still running after this is killed and read as a sample of this
/// length.
pub const DEADLINE: Duration = Duration::from_secs(10);
/// Outcomes kept: at four a minute, the last two minutes.
pub const WINDOW: usize = 9;
/// Samples the median needs before it is reported.
pub const MIN_SAMPLES: usize = 3;
/// Consecutive failed probes after which older samples no longer stand for the
/// present.
pub const FAILURE_RUN: usize = 3;
/// A window whose newest outcome is older than this is unavailable: four
/// cadences, so a probe thread that has stopped cannot leave a fast reading
/// standing.
pub const STALE_AFTER: Duration = Duration::from_secs(60);

/// The supervised thread's name and its Diagnostics subsystem.
const SUBSYSTEM: &str = "exec-probe";

/// What the probe reports: the median wall time of recent probes, how many
/// samples it rests on, and whether the latch calls that slow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ExecLatency {
    pub median_ms: u64,
    pub n: u32,
    /// Latched on the median ([`ENTER_MS`], [`EXIT_MS`]): what the governor
    /// decides on.
    pub slow: bool,
}

/// What one probe did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A wall time to exec and exit, or the deadline when it was hit.
    Sample(Duration),
    /// No number: refused, no slot, failed to spawn, or a non-zero exit.
    Failed,
}

/// The recent probes and the latch, advanced one probe at a time.
#[derive(Debug, Default)]
pub struct Window {
    /// Newest last: when the probe finished and its sample in milliseconds, or
    /// `None` for a failed probe.
    outcomes: VecDeque<(Instant, Option<u64>)>,
    slow: bool,
}

impl Window {
    /// Records one probe that finished at `at`.
    pub fn push(&mut self, at: Instant, outcome: Outcome) {
        let sample = match outcome {
            Outcome::Sample(took) => {
                Some(u64::try_from(took.as_millis()).unwrap_or(u64::MAX))
            }
            Outcome::Failed => None,
        };
        if self.outcomes.len() == WINDOW {
            self.outcomes.pop_front();
        }
        self.outcomes.push_back((at, sample));
        // The latch follows the median only once there is one, and only when
        // the median is outside the dead band.
        if let Some((median, _)) = self.median() {
            if median >= ENTER_MS {
                self.slow = true;
            } else if median < EXIT_MS {
                self.slow = false;
            }
        }
    }

    /// The median of the samples in the window and how many there are, once
    /// there are [`MIN_SAMPLES`]. An even count takes the mean of the middle
    /// two.
    fn median(&self) -> Option<(u64, usize)> {
        let mut samples: Vec<u64> = self.outcomes.iter().filter_map(|(_, s)| *s).collect();
        if samples.len() < MIN_SAMPLES {
            return None;
        }
        samples.sort_unstable();
        let n = samples.len();
        let median = if n % 2 == 1 {
            samples[n / 2]
        } else {
            ((u128::from(samples[n / 2 - 1]) + u128::from(samples[n / 2])) / 2) as u64
        };
        Some((median, n))
    }

    /// The reading at `now`: a value only when the window can say how long
    /// start-up takes now. Unavailable otherwise, and never fast.
    pub fn reading(&self, now: Instant) -> Reading<ExecLatency> {
        let Some((newest, _)) = self.outcomes.back() else {
            return Reading::Unavailable(Why::NotYet);
        };
        if now.saturating_duration_since(*newest) > STALE_AFTER {
            return Reading::Unavailable(Why::Failed);
        }
        let trailing_failures = self
            .outcomes
            .iter()
            .rev()
            .take_while(|(_, sample)| sample.is_none())
            .count();
        if trailing_failures >= FAILURE_RUN {
            return Reading::Unavailable(Why::Failed);
        }
        match self.median() {
            Some((median_ms, n)) => Reading::Value(ExecLatency {
                median_ms,
                n: u32::try_from(n).unwrap_or(u32::MAX),
                slow: self.slow,
            }),
            // Too few samples: still warming up when nothing has failed, a
            // failed check when something has.
            None if self.outcomes.iter().any(|(_, sample)| sample.is_none()) => {
                Reading::Unavailable(Why::Failed)
            }
            None => Reading::Unavailable(Why::NotYet),
        }
    }
}

/// Runs one probe with `run` and records it in `window` at the time `now`
/// reports once it has finished. The seam the loop and the tests share.
pub(super) fn probe_once(
    window: &Mutex<Window>,
    now: &dyn Fn() -> Instant,
    run: &mut dyn FnMut() -> Outcome,
) {
    let outcome = run();
    window
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .push(now(), outcome);
}

/// The running probe: its window, and the thread that feeds it.
pub struct ExecProbe {
    window: Arc<Mutex<Window>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl ExecProbe {
    /// The reading now, for the sampler to put on the governor's input.
    pub fn reading(&self) -> Reading<ExecLatency> {
        reading_of(&self.window)
    }

    /// A handle that reads the window without owning the thread.
    pub fn reader(&self) -> impl Fn() -> Reading<ExecLatency> + Send + 'static {
        let window = self.window.clone();
        move || reading_of(&window)
    }

    /// Stops the thread and waits for it. A probe in flight finishes first,
    /// bounded by [`DEADLINE`].
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

fn reading_of(window: &Mutex<Window>) -> Reading<ExecLatency> {
    window
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .reading(Instant::now())
}

/// The program the probe starts, or `None` where no trivial one is known
/// (Windows, or a Unix without `true` at the usual places): the reading is
/// then unsupported, which is silent, rather than a probe that cannot run.
fn probe_program() -> Option<std::path::PathBuf> {
    if !cfg!(unix) {
        return None;
    }
    ["/usr/bin/true", "/bin/true"]
        .into_iter()
        .map(std::path::PathBuf::from)
        .find(|path| path.is_file())
}

/// Starts the probe on its own supervised thread, or `None` when this
/// platform has no program to start. The first probe runs at once.
pub fn start() -> Option<ExecProbe> {
    let program = probe_program()?;
    let window = Arc::new(Mutex::new(Window::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let (body_window, body_stop) = (window.clone(), stop.clone());
    let thread = crate::supervisor::spawn_service(SUBSYSTEM, move || {
        while !body_stop.load(Ordering::Acquire) {
            let began = Instant::now();
            probe_once(&body_window, &Instant::now, &mut || {
                use crate::engine::process::ExecProbeRun;
                match crate::engine::process::time_exec_probe(&program, DEADLINE) {
                    ExecProbeRun::Ran(took) => Outcome::Sample(took),
                    ExecProbeRun::DeadlineHit => Outcome::Sample(DEADLINE),
                    ExecProbeRun::Failed => Outcome::Failed,
                }
            });
            // Parked rather than slept so a stop wakes it; spurious wakeups
            // just re-check the clock.
            while !body_stop.load(Ordering::Acquire) {
                let left = CADENCE.saturating_sub(began.elapsed());
                if left.is_zero() {
                    break;
                }
                std::thread::park_timeout(left);
            }
        }
    })
    .ok()?;
    Some(ExecProbe {
        window,
        stop,
        thread: Some(thread),
    })
}

#[cfg(test)]
mod tests;
