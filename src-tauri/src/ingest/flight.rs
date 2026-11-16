//! One budget for the whole catch-up, and one flight per repository.
//!
//! The pass's five-second deadline used to be polled only by the transcript
//! loops. Everything else — the `git worktree list` behind the repository's
//! address, the sibling-ledger consolidation, the reflog spawn, paging the
//! ledger for a baseline, walking the corpus, reading a transcript — ran
//! unbounded, and a catch-up was observed at 72 s with a 2 ms queue: the time
//! was work. Every phase now takes the same [`Budget`], and the caller waits for
//! the pass no longer than that budget however a phase stalls (a hung git, a
//! ledger another process holds locked). A pass the caller stopped waiting for
//! keeps running on its worker thread, still bound by the budget, and its
//! result is what the next caller inside the interval is handed.
//!
//! The same flight is what a tab switch joins instead of starting another
//! pass: `App.svelte` asks for a catch-up on every repository change, and the
//! daemon already runs the same pass every five minutes.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use super::CatchUp;

/// Where "now" comes from. Injectable so a test can move time without sleeping.
pub(crate) type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

/// A point in time past which no phase of the pass may start more work.
#[derive(Clone)]
pub(crate) struct Budget {
    limit: Option<Instant>,
    clock: Clock,
}

impl Budget {
    /// No limit: what tests and one-shot tools use.
    pub fn unbounded() -> Self {
        Self {
            limit: None,
            clock: Arc::new(Instant::now),
        }
    }

    /// `duration` from now on the system clock.
    pub fn within(duration: Duration) -> Self {
        Self::within_on(duration, Arc::new(Instant::now))
    }

    /// `duration` from now on `clock`.
    pub fn within_on(duration: Duration, clock: Clock) -> Self {
        let now = clock();
        Self {
            limit: now.checked_add(duration),
            clock,
        }
    }

    /// A budget that ends `percent` of the way from now to this one's limit,
    /// on the same clock: a phase's own slice, so that what it leaves unused
    /// belongs to the phases after it and what it overruns does not. Unbounded
    /// stays unbounded.
    pub fn share(&self, percent: u32) -> Self {
        let now = (self.clock)();
        Self {
            limit: self.limit.and_then(|limit| {
                let left = limit.saturating_duration_since(now);
                now.checked_add(left / 100 * percent.min(100))
            }),
            clock: Arc::clone(&self.clock),
        }
    }

    pub fn expired(&self) -> bool {
        self.limit.is_some_and(|limit| (self.clock)() >= limit)
    }

    /// Time left, or `None` when unbounded. Zero once expired.
    pub fn remaining(&self) -> Option<Duration> {
        self.limit
            .map(|limit| limit.saturating_duration_since((self.clock)()))
    }
}

/// The phase a pass is in, for naming what a stalled pass was stuck on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub(crate) enum Phase {
    Resolving = 0,
    Reflog = 1,
    Transcripts = 2,
    Pruning = 3,
    Done = 4,
}

impl Phase {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Phase::Resolving,
            1 => Phase::Reflog,
            2 => Phase::Transcripts,
            3 => Phase::Pruning,
            _ => Phase::Done,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Phase::Resolving => "resolving the worktree family",
            Phase::Reflog => "reading the reflog",
            Phase::Transcripts => "reading transcripts",
            Phase::Pruning => "pruning old events",
            Phase::Done => "finishing",
        }
    }
}

/// What a running pass has done so far, readable from the thread waiting on it.
#[derive(Default)]
pub(crate) struct Progress {
    phase: AtomicU8,
    counts: Mutex<CatchUp>,
}

impl Progress {
    pub fn enter(&self, phase: Phase) {
        self.phase.store(phase as u8, Ordering::Release);
    }

    pub fn phase(&self) -> Phase {
        Phase::from_u8(self.phase.load(Ordering::Acquire))
    }

    /// Replaces the counts with the pass's latest.
    pub fn publish(&self, counts: &CatchUp) {
        if let Ok(mut held) = self.counts.lock() {
            *held = counts.clone();
        }
    }

    /// The honest result for a pass nobody waited for: what it had done, and
    /// the phase it was in when the budget ran out. Never `truncated: false`.
    fn abandoned(&self, budget: Duration) -> CatchUp {
        let mut counts = self
            .counts
            .lock()
            .map(|held| held.clone())
            .unwrap_or_default();
        counts.truncated = true;
        let note = format!(
            "catch-up truncated: still {} after {} ms",
            self.phase().label(),
            budget.as_millis()
        );
        counts.error = if counts.error.is_empty() {
            note
        } else {
            format!("{}; {note}", counts.error)
        };
        counts
    }
}

struct Flight {
    done: Mutex<Option<CatchUp>>,
    ready: Condvar,
    progress: Progress,
}

struct Finished {
    at: Instant,
    result: CatchUp,
}

#[derive(Default)]
struct Slot {
    running: Option<Arc<Flight>>,
    last: Option<Finished>,
    /// Passes in a row that ended cut short or failed, ending at `last`.
    /// Zero after a pass that covered everything.
    incomplete_streak: u32,
}

type Slots = Arc<Mutex<HashMap<String, Slot>>>;

/// How long a finished pass answers for the repository before another may run.
#[derive(Clone, Copy)]
pub(crate) struct Intervals {
    /// After a pass that covered everything and reported no error.
    pub complete: Duration,
    /// After the first pass in a row that was cut short or failed, which has
    /// work still owed. Each further one in a row doubles it, up to
    /// `incomplete_cap`.
    pub incomplete: Duration,
    /// The longest the wait after a cut-short pass grows to.
    pub incomplete_cap: Duration,
}

impl Intervals {
    /// How long the last pass answers for, given how many passes in a row
    /// (`streak`, at least one for an incomplete result) ended incomplete.
    ///
    /// A pass that is always cut short is retried at this pace, not at the
    /// first interval for ever: each retry kills the git child the previous
    /// pass left running, so a fixed short interval is a loop that never
    /// finishes anything.
    fn after(&self, last: &CatchUp, streak: u32) -> Duration {
        if !(last.truncated || !last.error.is_empty()) {
            return self.complete;
        }
        let doublings = streak.saturating_sub(1).min(32);
        self.incomplete
            .saturating_mul(1u32 << doublings)
            .min(self.incomplete_cap.max(self.incomplete))
    }
}

/// The default: a tab switch inside a minute is answered by the last pass, and
/// a pass that keeps being cut short is retried after 10 s, 20 s, 40 s and so
/// on up to five minutes.
pub(crate) const INTERVALS: Intervals = Intervals {
    complete: Duration::from_secs(60),
    incomplete: Duration::from_secs(10),
    incomplete_cap: Duration::from_secs(300),
};

/// Single-flight and minimum-interval bookkeeping, per key.
pub(crate) struct Gate {
    slots: Slots,
    intervals: Intervals,
    clock: Clock,
}

impl Gate {
    pub fn new(intervals: Intervals) -> Self {
        Self::with_clock(intervals, Arc::new(Instant::now))
    }

    /// A gate whose intervals are measured on `clock`, so a test can move time
    /// without sleeping.
    pub fn with_clock(intervals: Intervals, clock: Clock) -> Self {
        Self {
            slots: Arc::new(Mutex::new(HashMap::new())),
            intervals,
            clock,
        }
    }

    /// The pass for `key`: answered from the last one inside its interval,
    /// joined if one is already running, started on a worker otherwise. The
    /// caller waits at most `wait`, and the worker's own budget is the same.
    pub fn run<F>(&self, key: &str, wait: Duration, work: F) -> CatchUp
    where
        F: FnOnce(&Budget, &Progress) -> CatchUp + Send + 'static,
    {
        let started = Instant::now();
        let flight = {
            let mut slots = self
                .slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let slot = slots.entry(key.to_string()).or_default();
            if slot.running.is_none() {
                if let Some(last) = &slot.last {
                    let interval = self.intervals.after(&last.result, slot.incomplete_streak);
                    if (self.clock)().saturating_duration_since(last.at) < interval {
                        return last.result.clone();
                    }
                }
            }
            match &slot.running {
                Some(running) => Arc::clone(running),
                None => {
                    let flight = Arc::new(Flight {
                        done: Mutex::new(None),
                        ready: Condvar::new(),
                        progress: Progress::default(),
                    });
                    if let Err(error) = spawn(&self.slots, &self.clock, key, &flight, wait, work) {
                        return CatchUp {
                            truncated: true,
                            error: format!("catch-up could not start: {error}"),
                            ..CatchUp::default()
                        };
                    }
                    slot.running = Some(Arc::clone(&flight));
                    flight
                }
            }
        };
        await_flight(&flight, wait.saturating_sub(started.elapsed()), wait)
    }
}

/// Starts the worker. It records its result in the gate's slot when it ends,
/// whether or not anyone is still waiting.
fn spawn<F>(
    slots: &Slots,
    clock: &Clock,
    key: &str,
    flight: &Arc<Flight>,
    wait: Duration,
    work: F,
) -> std::io::Result<()>
where
    F: FnOnce(&Budget, &Progress) -> CatchUp + Send + 'static,
{
    let slots = Arc::clone(slots);
    let clock = Arc::clone(clock);
    let flight = Arc::clone(flight);
    let key = key.to_string();
    // The worker catches its own panic and turns it into the pass's reported
    // error, so waiters are always woken; the supervisor adds the Diagnostics
    // record for anything that escapes that.
    crate::supervisor::spawn_once("catch-up", "catch-up", move || {
        let budget = Budget::within(wait);
        // A catch-up is housekeeping for the ledger, never the answer to a
        // click: its children are admitted as Background so that a pass over
        // dozens of worktrees cannot spend the reactive spawn budget the
        // interface reads from. The class is entered here, on the worker,
        // because a thread hop would otherwise start the pass as Reactive.
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::engine::process::with_background_processes(|| work(&budget, &flight.progress))
        }))
        .unwrap_or_else(|_| CatchUp {
            error: "catch-up panicked".into(),
            ..CatchUp::default()
        });
        flight.progress.enter(Phase::Done);
        // The slot is updated before the waiters are woken, so a caller
        // that returns and immediately asks again sees this result.
        {
            let mut slots = slots
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let slot = slots.entry(key).or_default();
            slot.running = None;
            // A pass that was cut short but moved the work forward (a ref read,
            // a transcript ingested) is a cold family converging, so it waits
            // the base interval; only a pass with nothing to show doubles it.
            slot.incomplete_streak = if !(result.truncated || !result.error.is_empty()) {
                0
            } else if result.made_progress() {
                1
            } else {
                slot.incomplete_streak.saturating_add(1)
            };
            slot.last = Some(Finished {
                at: clock(),
                result: result.clone(),
            });
        }
        *flight
            .done
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(result);
        flight.ready.notify_all();
    })
    .map(|_| ())
}

fn await_flight(flight: &Arc<Flight>, remaining: Duration, wait: Duration) -> CatchUp {
    let guard = flight
        .done
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let (guard, _) = flight
        .ready
        .wait_timeout_while(guard, remaining, |done| done.is_none())
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    match guard.as_ref() {
        Some(result) => result.clone(),
        None => flight.progress.abandoned(wait),
    }
}

/// `work` on a worker, waited for at most `wait`: the bound without the
/// single-flight or the interval, for a caller that wants its own fresh pass.
pub(crate) fn bounded<F>(wait: Duration, work: F) -> CatchUp
where
    F: FnOnce(&Budget, &Progress) -> CatchUp + Send + 'static,
{
    Gate::new(Intervals {
        complete: Duration::ZERO,
        incomplete: Duration::ZERO,
        incomplete_cap: Duration::ZERO,
    })
    .run("bounded", wait, work)
}

/// The process-wide gate.
pub(crate) fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(|| Gate::new(INTERVALS))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    fn quick() -> Intervals {
        Intervals {
            complete: Duration::from_secs(60),
            incomplete: Duration::from_secs(1),
            incomplete_cap: Duration::from_secs(1),
        }
    }

    /// A clock the test moves by hand.
    fn manual_clock() -> (Clock, Arc<Mutex<Duration>>) {
        let base = Instant::now();
        let offset = Arc::new(Mutex::new(Duration::ZERO));
        let held = Arc::clone(&offset);
        let clock: Clock = Arc::new(move || base + *held.lock().unwrap());
        (clock, offset)
    }

    #[test]
    fn a_budget_expires_when_its_clock_passes_the_limit() {
        let (clock, offset) = manual_clock();
        let budget = Budget::within_on(Duration::from_secs(5), clock);
        assert!(!budget.expired());
        assert_eq!(budget.remaining(), Some(Duration::from_secs(5)));
        *offset.lock().unwrap() = Duration::from_secs(4);
        assert!(!budget.expired());
        assert_eq!(budget.remaining(), Some(Duration::from_secs(1)));
        *offset.lock().unwrap() = Duration::from_secs(5);
        assert!(budget.expired());
        assert_eq!(budget.remaining(), Some(Duration::ZERO));
        assert_eq!(Budget::unbounded().remaining(), None);
        assert!(!Budget::unbounded().expired());
    }

    /// A tab switch asks for a catch-up each time. Ten switches between two
    /// repositories inside one minute are two passes, not ten.
    #[test]
    fn switching_between_two_repositories_ten_times_runs_two_passes() {
        let gate = Gate::new(quick());
        let runs = Arc::new(AtomicUsize::new(0));
        for switch in 0..10 {
            let key = if switch % 2 == 0 {
                "/repo/a"
            } else {
                "/repo/b"
            };
            let counted = Arc::clone(&runs);
            let result = gate.run(key, Duration::from_secs(5), move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                CatchUp {
                    recorded: 3,
                    ..CatchUp::default()
                }
            });
            assert_eq!(result.recorded, 3, "switch {switch} was not answered");
        }
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn a_caller_arriving_mid_flight_joins_it() {
        let gate = Arc::new(Gate::new(quick()));
        let runs = Arc::new(AtomicUsize::new(0));
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let counted = Arc::clone(&runs);
        let first = {
            let gate = Arc::clone(&gate);
            std::thread::spawn(move || {
                gate.run("/repo", Duration::from_secs(10), move |_, _| {
                    counted.fetch_add(1, Ordering::SeqCst);
                    hold.recv().ok();
                    CatchUp {
                        recorded: 7,
                        ..CatchUp::default()
                    }
                })
            })
        };
        while runs.load(Ordering::SeqCst) == 0 {
            std::thread::yield_now();
        }
        let joiner = {
            let gate = Arc::clone(&gate);
            let counted = Arc::clone(&runs);
            std::thread::spawn(move || {
                gate.run("/repo", Duration::from_secs(10), move |_, _| {
                    counted.fetch_add(100, Ordering::SeqCst);
                    CatchUp::default()
                })
            })
        };
        std::thread::sleep(Duration::from_millis(50));
        release.send(()).unwrap();
        assert_eq!(first.join().unwrap().recorded, 7);
        assert_eq!(joiner.join().unwrap().recorded, 7);
        assert_eq!(runs.load(Ordering::SeqCst), 1, "a second pass was started");
    }

    /// The honesty rule: a caller that stops waiting is told the pass was cut
    /// short, with the phase and what it had done, never that it completed.
    #[test]
    fn a_stalled_pass_returns_truncated_within_the_budget() {
        // Min of three: a loaded host can delay any one return, and the claim
        // is about what the code costs, not about the scheduler.
        let mut fastest = Duration::MAX;
        for attempt in 0..3 {
            let gate = Gate::new(quick());
            let (release, hold) = std::sync::mpsc::channel::<()>();
            let started = Instant::now();
            let result = gate.run("/repo", Duration::from_millis(200), move |_, progress| {
                progress.enter(Phase::Reflog);
                progress.publish(&CatchUp {
                    recorded: 4,
                    ..CatchUp::default()
                });
                hold.recv().ok();
                CatchUp::default()
            });
            fastest = fastest.min(started.elapsed());
            release.send(()).ok();
            assert!(result.truncated, "attempt {attempt}: {result:?}");
            assert!(
                result.error.contains("reading the reflog"),
                "attempt {attempt}: {result:?}"
            );
            assert_eq!(result.recorded, 4, "what the pass had done is carried");
        }
        assert!(
            fastest < Duration::from_millis(200 + 250),
            "the fastest of three returned after {fastest:?}"
        );
    }

    /// The abandoned worker's result is not lost: it is what the next caller
    /// inside the interval is handed.
    #[test]
    fn an_abandoned_pass_still_records_its_result() {
        let gate = Gate::new(Intervals {
            complete: Duration::from_secs(60),
            incomplete: Duration::from_secs(60),
            incomplete_cap: Duration::from_secs(60),
        });
        let (release, hold) = std::sync::mpsc::channel::<()>();
        let first = gate.run("/repo", Duration::from_millis(50), move |_, _| {
            hold.recv().ok();
            CatchUp {
                recorded: 9,
                ..CatchUp::default()
            }
        });
        assert!(first.truncated);
        release.send(()).unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let again = gate.run("/repo", Duration::from_millis(50), |_, _| CatchUp {
                recorded: -1,
                ..CatchUp::default()
            });
            if !again.truncated {
                assert_eq!(again.recorded, 9, "the finished pass was discarded");
                break;
            }
            assert!(Instant::now() < deadline, "the worker never finished");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A pass that was cut short has work owed, so it is retried sooner than a
    /// complete one is.
    #[test]
    fn an_incomplete_pass_is_retried_after_the_shorter_interval() {
        let gate = Gate::new(quick());
        let runs = Arc::new(AtomicUsize::new(0));
        let pass = |runs: &Arc<AtomicUsize>| {
            let counted = Arc::clone(runs);
            gate.run("/repo", Duration::from_secs(5), move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                CatchUp {
                    truncated: true,
                    error: "catch-up truncated".into(),
                    ..CatchUp::default()
                }
            })
        };
        pass(&runs);
        pass(&runs);
        assert_eq!(
            runs.load(Ordering::SeqCst),
            1,
            "retried inside the interval"
        );
        std::thread::sleep(Duration::from_millis(1200));
        pass(&runs);
        assert_eq!(runs.load(Ordering::SeqCst), 2);
    }

    /// One pass through `gate` on the manual clock's key `/repo`, counting runs
    /// and ending as `truncated` says.
    fn timed_pass(gate: &Gate, runs: &Arc<AtomicUsize>, truncated: bool) -> CatchUp {
        let counted = Arc::clone(runs);
        gate.run("/repo", Duration::from_secs(5), move |_, _| {
            counted.fetch_add(1, Ordering::SeqCst);
            CatchUp {
                truncated,
                error: if truncated {
                    "catch-up truncated".into()
                } else {
                    String::new()
                },
                ..CatchUp::default()
            }
        })
    }

    /// A pass that is cut short every time was retried every ten seconds for
    /// ever, and each retry killed the git child the last one left running.
    /// The wait doubles with each consecutive incomplete pass up to the cap,
    /// and a call inside it is answered from the last pass without a new one.
    #[test]
    fn consecutive_incomplete_passes_back_off_up_to_the_cap() {
        let (clock, offset) = manual_clock();
        let gate = Gate::with_clock(
            Intervals {
                complete: Duration::from_secs(60),
                incomplete: Duration::from_secs(10),
                incomplete_cap: Duration::from_secs(40),
            },
            clock,
        );
        let runs = Arc::new(AtomicUsize::new(0));
        let advance = |seconds: u64| {
            *offset.lock().unwrap() += Duration::from_secs(seconds);
        };
        let count = || runs.load(Ordering::SeqCst);

        timed_pass(&gate, &runs, true);
        assert_eq!(count(), 1);
        // Waits after the first, second, third and fourth incomplete pass:
        // 10 s, 20 s, 40 s, then held at the cap of 40 s.
        for (pass, wait) in [(2, 10), (3, 20), (4, 40), (5, 40)] {
            advance(wait - 1);
            let answered = timed_pass(&gate, &runs, true);
            assert!(answered.truncated);
            assert_eq!(count(), pass - 1, "pass {pass} started {wait} s early");
            advance(1);
            timed_pass(&gate, &runs, true);
            assert_eq!(count(), pass, "pass {pass} did not start after {wait} s");
        }
    }

    /// The backoff is owed to the work, so a pass that finishes ends it: the
    /// next incomplete pass is retried after the first interval again.
    #[test]
    fn a_completed_pass_resets_the_backoff() {
        let (clock, offset) = manual_clock();
        let gate = Gate::with_clock(
            Intervals {
                complete: Duration::from_secs(60),
                incomplete: Duration::from_secs(10),
                incomplete_cap: Duration::from_secs(300),
            },
            clock,
        );
        let runs = Arc::new(AtomicUsize::new(0));
        let advance = |seconds: u64| {
            *offset.lock().unwrap() += Duration::from_secs(seconds);
        };
        let count = || runs.load(Ordering::SeqCst);

        timed_pass(&gate, &runs, true);
        advance(10);
        timed_pass(&gate, &runs, true);
        advance(20);
        let done = timed_pass(&gate, &runs, false);
        assert!(!done.truncated);
        assert_eq!(count(), 3);
        // A complete pass answers for a minute.
        advance(59);
        timed_pass(&gate, &runs, true);
        assert_eq!(count(), 3);
        advance(1);
        let cut = timed_pass(&gate, &runs, true);
        assert!(cut.truncated);
        assert_eq!(count(), 4);
        // The streak began again: ten seconds, not the forty the earlier two
        // incomplete passes would have owed.
        advance(10);
        timed_pass(&gate, &runs, true);
        assert_eq!(count(), 5, "the backoff survived a completed pass");
    }

    /// A pass cut short that nonetheless did work (a transcript ingested, a ref
    /// read) is a cold family converging, not a stuck one: it keeps the base
    /// interval. Only a pass with nothing to show for it doubles the wait.
    #[test]
    fn only_incomplete_passes_without_progress_back_off() {
        let (clock, offset) = manual_clock();
        let gate = Gate::with_clock(
            Intervals {
                complete: Duration::from_secs(60),
                incomplete: Duration::from_secs(10),
                incomplete_cap: Duration::from_secs(300),
            },
            clock,
        );
        let runs = Arc::new(AtomicUsize::new(0));
        let advance = |seconds: u64| {
            *offset.lock().unwrap() += Duration::from_secs(seconds);
        };
        let count = || runs.load(Ordering::SeqCst);
        let pass = |progress: bool| {
            let counted = Arc::clone(&runs);
            gate.run("/repo", Duration::from_secs(5), move |_, _| {
                counted.fetch_add(1, Ordering::SeqCst);
                CatchUp {
                    truncated: true,
                    error: "catch-up truncated".into(),
                    transcripts: i64::from(progress),
                    ..CatchUp::default()
                }
            })
        };

        pass(true);
        for expected in 2..=6 {
            advance(9);
            pass(true);
            assert_eq!(count(), expected - 1, "pass {expected} started early");
            advance(1);
            pass(true);
            assert_eq!(
                count(),
                expected,
                "pass {expected}: progress was made, so the base interval applies"
            );
        }
        // Passes with nothing to show double the wait from the base: 10 s
        // after the last pass that made progress, then 20 s, then 40 s.
        advance(10);
        pass(false);
        assert_eq!(count(), 7);
        advance(19);
        pass(false);
        assert_eq!(count(), 7, "no progress, so the wait doubled to 20 s");
        advance(1);
        pass(false);
        assert_eq!(count(), 8);
        advance(39);
        pass(false);
        assert_eq!(count(), 8, "no progress, so the wait doubled to 40 s");
        advance(1);
        pass(false);
        assert_eq!(count(), 9);
    }
}
