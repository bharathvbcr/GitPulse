//! Coalescing identical bounded subprocess reads in flight.

use super::gate::{
    current_admission, spawn_gate, Admission, SpawnGate, SLOT_WAIT_SUFFIX, TIMEOUT_MARKER,
};
use super::read_cancel;
use super::runner::{
    is_cancellation, is_timed_out, keeping_deadline_prefix, note_process_failure, run_admitted,
    run_with_gate, BoundedRun, Incomplete, ProcessObserver,
};
use crate::ipc_error::CommandError;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// What makes two reads the same read: where, what, how much output, and the
/// class asking. The class is part of it so a user action never inherits a
/// refresh's refusal.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(super) struct SharedReadKey {
    pub(super) cwd: PathBuf,
    program: std::ffi::OsString,
    argv: Vec<std::ffi::OsString>,
    stdout_cap: usize,
    class: Admission,
}

/// A queued read that later identical reads may join. It leaves the table
/// the moment the gate admits it, before the child starts: a caller that
/// arrives after that point may be asking because of a write the running
/// child cannot have seen, so it starts its own.
pub(super) struct SharedRead {
    result: Mutex<Option<Result<BoundedRun, CommandError>>>,
    done: Condvar,
    /// Every request that has waited on this flight, the leader's first. A
    /// request that named no id waits for good while it is here, so a flight
    /// with one is never cancelled; otherwise the flight is cancelled when
    /// every entry is gone: its flag set, or the request having left by its own
    /// deadline (see [`abandon_if_all_gone`], [`Waiter`]). Pushed only while
    /// the table is held, and never removed from, so an index stays valid.
    waiters: Mutex<Vec<Waiter>>,
    /// Set, with the flight leaving the table, by the decision to cancel it.
    /// A flight that is abandoned is never joined: a caller that arrives
    /// later, however it found the flight, starts a read of its own.
    abandoned: std::sync::atomic::AtomicBool,
    /// Where this flight's joiners go when its leader's deadline passed
    /// before theirs: the one flight that asks again for all of them, set by
    /// the first to arrive. It stays reachable from here after the gate has
    /// admitted it, because everyone who asks was waiting before its child
    /// started, which is all the "joined while queued" rule protects.
    retry: Mutex<Option<Arc<SharedRead>>>,
}

/// One request's stake in a flight.
enum Waiter {
    /// Still waiting for the answer. `None` is a request with no id: it cannot
    /// be cancelled, so it counts as present until it leaves.
    Waiting(Option<read_cancel::ReadCancel>),
    /// A joiner that stopped waiting on its own (its deadline, a hand-off to a
    /// retry flight, an answer received, an unwind). Departure is recorded
    /// here and never by cancelling the request's flag: that flag belongs to
    /// the whole request and may be shared with other reads of it.
    Left,
}

impl Waiter {
    fn is_gone(&self) -> bool {
        match self {
            Self::Waiting(cancel) => cancel
                .as_ref()
                .is_some_and(read_cancel::ReadCancel::is_cancelled),
            Self::Left => true,
        }
    }
}

/// Marks a joiner's entry `Left` on every way out of its wait. It takes only
/// the waiters lock, never the table, so it cannot invert the order
/// [`abandon_if_all_gone`] takes them in.
struct JoinerDeparture<'a> {
    flight: &'a SharedRead,
    index: usize,
}

impl Drop for JoinerDeparture<'_> {
    fn drop(&mut self) {
        let mut waiters = self
            .flight
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(slot) = waiters.get_mut(self.index) {
            *slot = Waiter::Left;
        }
    }
}

impl SharedRead {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            result: Mutex::new(None),
            done: Condvar::new(),
            waiters: Mutex::new(Vec::new()),
            abandoned: std::sync::atomic::AtomicBool::new(false),
            retry: Mutex::new(None),
        })
    }
}

/// How often a joiner looks at its own cancel flag while it waits for the
/// leader's answer.
const JOINER_CANCEL_POLL: Duration = Duration::from_millis(20);

/// Cancels `flight` if every request waiting on it has been cancelled, and
/// says whether it is (now or already) cancelled.
///
/// The decision and the flight's leaving the table are one step under the
/// table lock, which is also what a joiner holds while it joins and registers
/// itself: a joiner is either counted here, and the flight lives, or it comes
/// after and finds no flight, and starts a read of its own. Nobody inherits a
/// cancellation.
fn abandon_if_all_gone(key: &SharedReadKey, flight: &Arc<SharedRead>) -> bool {
    use std::sync::atomic::Ordering;
    if flight.abandoned.load(Ordering::Acquire) {
        return true;
    }
    let all_gone = |waiters: &[Waiter]| waiters.iter().all(Waiter::is_gone);
    // Without the table: nearly every poll of a flight someone still wants.
    if !all_gone(
        &flight
            .waiters
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner),
    ) {
        return false;
    }
    let mut reads = shared_reads()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let waiters = flight
        .waiters
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if !all_gone(&waiters) {
        return false;
    }
    flight.abandoned.store(true, Ordering::Release);
    if reads
        .get(key)
        .is_some_and(|entry| Arc::ptr_eq(entry, flight))
    {
        reads.remove(key);
    }
    true
}

/// The leader's observer: the child stops when nobody waits for its answer.
struct FlightObserver<'a> {
    key: &'a SharedReadKey,
    flight: &'a Arc<SharedRead>,
}

impl ProcessObserver for FlightObserver<'_> {
    fn cancelled(&self) -> bool {
        abandon_if_all_gone(self.key, self.flight)
    }

    fn cancellation(&self) -> Option<CommandError> {
        Some(read_cancel::cancelled())
    }
}

pub(super) fn shared_reads() -> &'static Mutex<HashMap<SharedReadKey, Arc<SharedRead>>> {
    static READS: OnceLock<Mutex<HashMap<SharedReadKey, Arc<SharedRead>>>> = OnceLock::new();
    READS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Runs a read-only `cmd`, sharing one child with every identical read still
/// queued at the gate. Only for commands that cannot change repository state:
/// a shared write would report one write's outcome as another's. Commands
/// without a working directory, or with stdin, are never shared.
///
/// `timeout` is this caller's own budget, whichever side of the flight it is
/// on. A joiner stops waiting for the leader's child when its own deadline
/// passes, with the timeout a solo call would report, and the leader runs on
/// untouched. A leader's timeout is the leader's: a joiner with budget left
/// runs its own read for what remains instead of adopting it.
/// Coalesces identical reads against the process-wide gate.
pub(crate) fn run_read_shared(
    cmd: Command,
    label: &str,
    timeout: Duration,
    stdout_cap: usize,
) -> Result<BoundedRun, CommandError> {
    run_read_shared_with(cmd, label, timeout, stdout_cap, spawn_gate())
}

pub(super) fn run_read_shared_with(
    mut cmd: Command,
    label: &str,
    timeout: Duration,
    stdout_cap: usize,
    gate: &'static SpawnGate,
) -> Result<BoundedRun, CommandError> {
    let deadline = Instant::now() + timeout;
    let mut attempt = timeout;
    // The request this thread is serving, if its caller can cancel it.
    let cancel = read_cancel::current();
    // The flight whose leader timed out last, and the longest prefix any such
    // leader printed: a keeping caller's rerun may reach its deadline with
    // less than that.
    let mut after: Option<Arc<SharedRead>> = None;
    let mut prefix: Option<BoundedRun> = None;
    loop {
        let outcome = match run_read_shared_once(
            &mut cmd,
            label,
            attempt,
            deadline,
            stdout_cap,
            gate,
            after.as_ref(),
            cancel.as_ref(),
        ) {
            SharedAttempt::Done(result) => result,
            SharedAttempt::LeaderTimedOut { flight, printed } => {
                if let Some(run) = printed {
                    if prefix
                        .as_ref()
                        .is_none_or(|kept| run.stdout.len() > kept.stdout.len())
                    {
                        prefix = Some(run);
                    }
                }
                after = Some(flight);
                attempt = deadline.saturating_duration_since(Instant::now());
                if attempt.is_zero() {
                    deadline_expired(label, timeout)
                } else {
                    continue;
                }
            }
        };
        return if after.is_some() {
            as_the_callers_deadline(outcome, prefix, label, timeout)
        } else {
            outcome
        };
    }
}

/// How one pass through the shared-read queue ended.
enum SharedAttempt {
    Done(Result<BoundedRun, CommandError>),
    /// The leader this caller joined ran out of its own time while this
    /// caller's deadline has not come. Its timeout is not this caller's
    /// answer, so the caller asks again, on `flight`'s retry. `printed` is
    /// what the leader had read when it was stopped.
    LeaderTimedOut {
        flight: Arc<SharedRead>,
        printed: Option<BoundedRun>,
    },
}

/// A caller that asked again after a leader's timeout ends at its own
/// deadline like a solo call: naming the budget it was given rather than the
/// remainder a rerun was handed, and, when it keeps prefixes, with the
/// longest prefix it saw, since a solo call over the same budget would have
/// read at least that much.
fn as_the_callers_deadline(
    outcome: Result<BoundedRun, CommandError>,
    prefix: Option<BoundedRun>,
    label: &str,
    timeout: Duration,
) -> Result<BoundedRun, CommandError> {
    if !is_deadline_outcome(&outcome) {
        return outcome;
    }
    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
    let longer = |own: usize| {
        prefix
            .as_ref()
            .filter(|_| keeping_deadline_prefix())
            .is_some_and(|kept| kept.stdout.len() > own)
    };
    match outcome {
        Ok(run) if !longer(run.stdout.len()) => Ok(rename_deadline(run, message)),
        Err(error) if !longer(0) => {
            // A refusal to wait any longer for a slot says so; only the bare
            // timeout is rewritten.
            Err(if error.message().ends_with(SLOT_WAIT_SUFFIX) {
                error
            } else {
                message.into()
            })
        }
        // The leader's prefix outweighs this caller's own.
        _ => match prefix {
            Some(kept) => Ok(rename_deadline(kept, message)),
            None => Err(message.into()),
        },
    }
}

fn rename_deadline(mut run: BoundedRun, message: String) -> BoundedRun {
    if let Some(Incomplete::Deadline { message: slot, .. }) = &mut run.incomplete {
        *slot = message;
    }
    run
}

/// Whether a flight's outcome is its leader's deadline rather than an answer.
fn is_deadline_outcome(outcome: &Result<BoundedRun, CommandError>) -> bool {
    match outcome {
        Ok(run) => matches!(run.incomplete, Some(Incomplete::Deadline { .. })),
        Err(error) => is_timed_out(error.message()),
    }
}

/// What a solo call that ran out of `timeout` reports: the prefix form for a
/// caller keeping deadline prefixes, the error otherwise (see
/// [`run_admitted`]).
fn deadline_expired(label: &str, timeout: Duration) -> Result<BoundedRun, CommandError> {
    let message = format!("{label}{TIMEOUT_MARKER}{}s", timeout.as_secs_f64());
    if keeping_deadline_prefix() {
        return Ok(BoundedRun {
            stdout: Vec::new(),
            stderr: Vec::new(),
            success: false,
            status_code: -1,
            signal: None,
            incomplete: Some(Incomplete::Deadline {
                message,
                over_cap: None,
            }),
            stderr_incomplete: None,
            cancelled: false,
        });
    }
    let error = CommandError::from(message);
    note_process_failure(&error);
    Err(error)
}

#[allow(clippy::too_many_arguments)]
fn run_read_shared_once(
    cmd: &mut Command,
    label: &str,
    timeout: Duration,
    deadline: Instant,
    stdout_cap: usize,
    gate: &'static SpawnGate,
    after: Option<&Arc<SharedRead>>,
    cancel: Option<&read_cancel::ReadCancel>,
) -> SharedAttempt {
    let Some(cwd) = cmd.get_current_dir().map(Path::to_path_buf) else {
        // Never shared, so its one waiter is its own request.
        let mut own = cancel.map(read_cancel::OwnCancel);
        let observer: &mut dyn ProcessObserver = match own.as_mut() {
            Some(observer) => observer,
            None => &mut (),
        };
        return SharedAttempt::Done(run_with_gate(
            cmd, label, timeout, None, stdout_cap, observer, gate,
        ));
    };
    let key = SharedReadKey {
        cwd,
        program: cmd.get_program().to_os_string(),
        argv: cmd.get_args().map(std::ffi::OsStr::to_os_string).collect(),
        stdout_cap,
        class: current_admission(),
    };
    let table = shared_reads();
    let (flight, leader, index) = {
        let mut reads = table
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Callers whose leader timed out share one retry flight, however
        // far its child has got, so a hung read meets one new child and not
        // one per caller. The table is held, so the first to arrive decides.
        let mut retry = after.map(|previous| {
            previous
                .retry
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        });
        // A flight that was cancelled is joined by no one, however it is found.
        let live =
            |flight: &Arc<SharedRead>| !flight.abandoned.load(std::sync::atomic::Ordering::Acquire);
        let known = retry
            .as_deref()
            .and_then(Clone::clone)
            .filter(live)
            .or_else(|| reads.get(&key).cloned().filter(live));
        let (flight, leader) = match known {
            Some(flight) => (flight, false),
            None => {
                let flight = SharedRead::new();
                reads.insert(key.clone(), Arc::clone(&flight));
                (flight, true)
            }
        };
        if let Some(slot) = retry.as_deref_mut() {
            if slot.as_ref().is_none_or(|held| !live(held)) {
                *slot = Some(Arc::clone(&flight));
            }
        }
        // Counted as a waiter in the same step that found the flight, so the
        // decision to cancel it (also under the table) sees this request.
        let index = {
            let mut waiters = flight
                .waiters
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            waiters.push(Waiter::Waiting(cancel.cloned()));
            waiters.len() - 1
        };
        (flight, leader, index)
    };
    if !leader {
        // Whichever way this joiner stops waiting, it stops counting: a leader
        // cancelled after that must be able to abandon its child.
        let _departure = JoinerDeparture {
            flight: &flight,
            index,
        };
        gate.note_coalesced(key.class);
        let mut result = flight
            .result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The leader always publishes, even when it panics (`Publish`), but
        // its child may outlast this caller's deadline: wait no longer than
        // that. An answer that is there wins over a deadline that has just
        // passed. A joiner whose own request is cancelled leaves at once and
        // the leader runs on for whoever still waits (the leader stops only
        // when all of them have left, see [`abandon_if_all_gone`]).
        let mut left_by_cancel = false;
        let shared = loop {
            if let Some(shared) = result.as_ref() {
                break Some(shared.clone());
            }
            if cancel.is_some_and(read_cancel::ReadCancel::is_cancelled) {
                left_by_cancel = true;
                break None;
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break None;
            }
            let slice = if cancel.is_some() {
                left.min(JOINER_CANCEL_POLL)
            } else {
                left
            };
            result = flight
                .done
                .wait_timeout(result, slice)
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .0;
        };
        drop(result);
        let Some(shared) = shared else {
            if left_by_cancel {
                return SharedAttempt::Done(Err(read_cancel::cancelled()));
            }
            return SharedAttempt::Done(deadline_expired(label, timeout));
        };
        if is_deadline_outcome(&shared) && Instant::now() < deadline {
            // Not answered by another's child after all.
            gate.note_uncoalesced(key.class);
            return SharedAttempt::LeaderTimedOut {
                flight: Arc::clone(&flight),
                printed: shared.ok(),
            };
        }
        // The leader counted its own failure on its thread; a joiner that
        // received it saw no answer either.
        if let Some(error) = shared
            .as_ref()
            .err()
            .filter(|error| !is_cancellation(error))
        {
            note_process_failure(error);
        }
        return SharedAttempt::Done(shared);
    }

    /// Closes the table entry and publishes whatever the leader ended with,
    /// so a leader that panics or is refused cannot strand its joiners.
    struct Publish<'a> {
        key: &'a SharedReadKey,
        flight: &'a Arc<SharedRead>,
        label: &'a str,
        outcome: Option<Result<BoundedRun, CommandError>>,
    }
    impl Drop for Publish<'_> {
        fn drop(&mut self) {
            close_shared_read(self.key, self.flight);
            let outcome = self.outcome.take().unwrap_or_else(|| {
                Err(format!("{} shared read ended without a result", self.label).into())
            });
            *self
                .flight
                .result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(outcome);
            self.flight.done.notify_all();
        }
    }
    let mut publish = Publish {
        key: &key,
        flight: &flight,
        label,
        outcome: None,
    };
    // The leader's observer is the flight's, not its own request's: the child
    // serves every waiter, so it stops when none is left.
    let mut observer = FlightObserver {
        key: &key,
        flight: &flight,
    };
    let outcome = run_admitted(
        cmd,
        label,
        super::runner::RunOptions::new(timeout),
        None,
        stdout_cap,
        &mut observer,
        gate,
        &|| close_shared_read(&key, &flight),
    );
    publish.outcome = Some(outcome.clone());
    drop(publish);
    SharedAttempt::Done(outcome)
}

/// Removes `flight` from the table if it is still the entry for `key`; a
/// later flight under the same key is left alone.
fn close_shared_read(key: &SharedReadKey, flight: &Arc<SharedRead>) {
    let mut reads = shared_reads()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if reads
        .get(key)
        .is_some_and(|entry| Arc::ptr_eq(entry, flight))
    {
        reads.remove(key);
    }
}
