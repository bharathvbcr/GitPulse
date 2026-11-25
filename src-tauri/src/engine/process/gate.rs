//! Process spawn concurrency and rate admission gate.

use super::clock::{Clock, SystemClock};
use super::shared_budget;
use crate::ipc_error::CommandError;
use std::cell::Cell;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// The exact phrase [`run_bounded`] embeds when a deadline kills a child and
/// the only marker [`run_captured`] matches to classify a timeout. Formatter
/// and matcher both go through this constant, so the two sides of the
/// stringly-typed seam cannot drift apart silently; the regression test
/// additionally pins the whole behavior end to end.
pub(crate) const TIMEOUT_MARKER: &str = " timed out after ";

/// The phrase a [`Refusal::Refused`] or [`Refusal::Shed`] error carries, and
/// the only thing [`is_deferred_under_load`] matches. A deferral is the app
/// declining to start more work under load, not git failing: it must never
/// carry [`TIMEOUT_MARKER`], or every reader of the error calls it a hang.
const DEFERRED_MARKER: &str = " deferred under load after ";

/// `err` without the one optional `[code] ` prefix the ledger adds. Every
/// anchored check on a runner message goes through this, so none can be
/// defeated by the prefix the others tolerate.
pub(super) fn strip_code_prefix(err: &str) -> &str {
    match err.strip_prefix('[').and_then(|rest| rest.split_once("] ")) {
        Some((code, rest)) if code.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') => rest,
        _ => err,
    }
}

/// True when `err` is the gate declining to start a child under load. The
/// command may succeed if asked again once load falls; it did not run.
///
/// Anchored to the one shape [`refusal_message`] writes, so text that merely
/// quotes the phrase (a path inside "Not a Git repository: <path>", a worktree
/// named in "does not belong") is not a deferral. The shape is, after an
/// optional `[code] ` prefix the ledger adds: a label of plain words (the
/// program and subcommand, never a path or a quote), [`DEFERRED_MARKER`], then
/// the wait as `<seconds>s:`. The label ends at the FIRST marker, so a hostile
/// path that carries a complete refusal later in the text cannot qualify.
pub(crate) fn is_deferred_under_load(err: &str) -> bool {
    let body = strip_code_prefix(err);
    let Some((label, after)) = body.split_once(DEFERRED_MARKER) else {
        return false;
    };
    let plain_label = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_' | '.'));
    let waited = after.split_once("s:").is_some_and(|(seconds, _)| {
        let (whole, fraction) = seconds.split_once('.').unwrap_or((seconds, "0"));
        [whole, fraction]
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
    });
    plain_label && waited
}

/// True when `err` is a child that never got a concurrency slot inside its
/// own deadline — as opposed to one that started and then ran out of time.
pub(crate) fn is_slot_wait_timeout(err: &str) -> bool {
    err.contains(TIMEOUT_MARKER) && err.contains(SLOT_WAIT_SUFFIX)
}

pub(crate) const SLOT_WAIT_SUFFIX: &str = "s waiting for a process slot";

/// Which of the gate's two rate buckets refused a spawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefusingBucket {
    /// This process's own bucket (the process gate).
    Local,
    /// The cross-process record every GitPulse process of the user draws from.
    Shared,
}

/// What a refusing bucket held when it refused: it admits a spawn only while
/// more than `floor` tokens remain (`0` for reactive work, the reserve kept
/// for reactive reads for background work), and refills up to `burst`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BucketLevel {
    pub bucket: RefusingBucket,
    pub tokens: u32,
    pub floor: u32,
    pub burst: u32,
}

impl BucketLevel {
    /// `shared bucket: 0 tokens, needs more than 48, burst 192`.
    fn describe(&self) -> String {
        let bucket = match self.bucket {
            RefusingBucket::Local => "local",
            RefusingBucket::Shared => "shared",
        };
        format!(
            "{bucket} bucket: {} token(s), needs more than {}, burst {}",
            self.tokens, self.floor, self.burst
        )
    }
}

#[cfg(test)]
pub(crate) fn refusal_message(label: &str, refusal: Refusal) -> CommandError {
    refusal_message_at(label, refusal, None)
}

/// What a spawn refused because its caller cancelled while it queued says
/// after its label.
pub(super) const CANCELLED_BEFORE_SPAWN: &str = " cancelled before spawn";

/// [`refusal_message`], naming the bucket that refused and its level when the
/// gate knows them. The text before the level is unchanged, so every reader
/// that matches on the deferral (see [`is_deferred_under_load`]) reads it as
/// before; the level is a parenthesised tail after the sentence.
pub(crate) fn refusal_message_at(
    label: &str,
    refusal: Refusal,
    level: Option<BucketLevel>,
) -> CommandError {
    let by = level.map_or_else(String::new, |level| format!(" ({})", level.describe()));
    // A deferral is typed where it is written (see `crate::ipc_error`): the
    // error carries the kind to the command's edge, and nothing reads the
    // marker back out of a message.
    let deferred =
        |text: String| CommandError::typed(crate::ipc_error::IpcErrorKind::DeferredUnderLoad, text);
    match refusal {
        Refusal::Cancelled => format!("{label}{CANCELLED_BEFORE_SPAWN}").into(),
        Refusal::TimedOut { deadline } => format!(
            "{label}{TIMEOUT_MARKER}{:.3}{SLOT_WAIT_SUFFIX}",
            deadline.as_secs_f64()
        )
        .into(),
        Refusal::Refused { waited } => deferred(format!(
            "{label}{DEFERRED_MARKER}{:.3}s: the git spawn rate limit admitted nothing sooner{by}",
            waited.as_secs_f64()
        )),
        Refusal::Shed => deferred(format!(
            "{label}{DEFERRED_MARKER}0.000s: background work is shed while the git spawn rate \
             budget is low{by}"
        )),
        Refusal::Pressure { reactive, cause } => {
            let cause = cause.unwrap_or(crate::sysload::Binding::MemoryPressure);
            // Slow start-up is the one cause with a number worth quoting in
            // the sentence: how slow. It follows the phrase, after the marker
            // every reader matches on.
            let measured = match cause {
                crate::sysload::Binding::ExecLatency => crate::sysload::exec_latency()
                    .map(|latency| {
                        format!(
                            " (median {})",
                            crate::sysload::format_latency(latency.median_ms)
                        )
                    })
                    .unwrap_or_default(),
                _ => String::new(),
            };
            deferred(format!(
                "{label}{DEFERRED_MARKER}0.000s: {} work is shed while {}{measured}",
                if reactive { "reactive" } else { "background" },
                cause.cause_phrase()
            ))
        }
    }
}

/// Ceiling on how many child processes this engine keeps alive at once.
///
/// Every `git`, `gh` and `npm` invocation in the app funnels through
/// [`run_bounded`], and each live one costs the parent two pipe descriptors,
/// up to two fallback drain threads off Unix, and up to
/// `MAX_OUTPUT_BYTES + 4 MiB` of buffered output.
/// Nothing bounded how many could be in flight at once, and three layers above
/// this one are happy to ask for hundreds: Tauri's blocking pool admits 512
/// concurrent tasks (tokio's default `max_blocking_threads`), `off_thread`
/// hands every command straight to it, and a workspace-wide refresh fans out
/// over as many as `MAX_BULK_TARGETS` repositories with five commands each.
///
/// The host says no long before the app does. A GUI launch inherits launchd's
/// soft `RLIMIT_NOFILE` of 256, so a few dozen simultaneous children exhaust
/// the descriptor table and *every* subsequent spawn fails with EMFILE —
/// surfacing as the "Failed to spawn git ...: Too many open files (os error
/// 24)" storm, which does not clear on its own because the UI retries into the
/// same wall. [`crate::limits::raise_open_file_limit`] buys the headroom back;
/// this gate is what stops the app from spending it all at once.
///
/// The permit spans the child's whole life, not just the spawn call, because
/// that is how long its descriptors, threads and buffers are held.
const SPAWN_LIMIT_FLOOR: usize = 4;
const SPAWN_LIMIT_CEILING: usize = 16;

/// Who is asking for a child, which decides what the gate may refuse it.
///
/// The measured storm on 2026-10-02 was a watcher-triggered refresh, and at
/// launch the rate cap that stopped it also refused the user's own actions,
/// because the gate could not tell the two apart. The class is the answer to
/// "what may be refused": a refresh can be deferred, a click must not be.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Admission {
    /// A user action: a mutation behind the command guard. Concurrency-limited
    /// only. It never waits for, and never spends, a rate token, so no amount
    /// of refresh traffic can refuse the action that caused it.
    Interactive,
    /// The default for every command and thread: the reads a `repo-changed`
    /// event, a poll or a tab switch asks for. Concurrency- and rate-limited,
    /// and identical queued reads share one child (see [`run_read_shared`]).
    Reactive,
    /// Indexing, docs and live-index work. At most a quarter of the slots, and
    /// shed — refused without waiting — while the rate budget is at or below
    /// the reserve kept for reactive reads.
    Background,
}

impl Admission {
    pub(super) const ALL: [Admission; 3] = [Self::Interactive, Self::Reactive, Self::Background];

    pub(super) fn index(self) -> usize {
        match self {
            Self::Interactive => 0,
            Self::Reactive => 1,
            Self::Background => 2,
        }
    }

    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Interactive => "interactive",
            Self::Reactive => "reactive",
            Self::Background => "background",
        }
    }
}

/// Whether every thread of this process starts in [`Admission::Background`].
///
/// The app leaves it unset. `gitpulse-mcp` and `gitpulsed` set it once, first
/// thing in `main`: their whole job is optional work on behalf of an agent or
/// a schedule, so under load it should be shed before the app the user is
/// looking at is deferred. A process-wide default rather than a scope, because
/// a scope ends at a thread hop — a request worker, or rayon's pool behind a
/// `par_iter` — and every hop would otherwise be promoted to `Reactive`.
static PROCESS_BACKGROUND: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Makes [`Admission::Background`] the class every thread of this process
/// starts in. Call before any thread is started; a thread that has already
/// asked keeps the class it was given.
pub fn run_process_as_background() {
    PROCESS_BACKGROUND.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Whether background spawns in this process wait for the rate budget instead
/// of being shed (see [`run_process_awaiting_background_budget`]).
static PROCESS_BACKGROUND_AWAITS: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Background spawns of this process wait, for at most [`SPAWN_QUEUE_BUDGET`],
/// for the budget to climb back above the background reserve, rather than
/// being shed at once. Call before any thread is started, like
/// [`run_process_as_background`].
///
/// Admission class of an agent-initiated MCP read: **Background, waiting**.
/// It is not Reactive, because the reserve exists so that the edit-time hook
/// (Reactive, gating the user's own tool call) and the app's reads keep budget
/// while agent traffic is heavy; fifteen agent sessions each taking an
/// insights snapshot (about 20 spawns on 18 worktrees, measured) would
/// otherwise spend the shared record to zero and get hooks refused. It is not
/// plain Background, because that sheds at 0.000s whenever the shared or local
/// budget sits at or below the reserve, and a caller blocked on the answer then
/// gets a refusal for a condition that clears in a fraction of a second at 8
/// tokens a second. Waiting keeps the floor (this class never spends the
/// reserve, so the shared budget still bounds it exactly as before) and turns
/// "shed while low" into "paced at the refill rate", bounded by the same 2 s
/// wait a Reactive read gets. Scheduled work (`gitpulsed`) keeps shedding.
pub fn run_process_awaiting_background_budget() {
    PROCESS_BACKGROUND_AWAITS.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by [`run_process_with_unlimited_spawn_rate`]; never set in a shipped
/// binary.
static PROCESS_UNLIMITED_RATE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Lifts the spawn *rate* cap for the rest of this process. The concurrency
/// limit still applies.
///
/// For integration-test processes only. Each `tests/*.rs` crate links the
/// library without `cfg(test)`, so it otherwise runs under the production
/// rate; a stress test that starts hundreds of children then measures that
/// rate instead of the code under test. (Such a process never reaches the
/// per-user shared budget either way: only the shipped binaries opt into it,
/// see [`run_process_with_shared_spawn_budget`].) The unit-test build gets
/// the same gate from `cfg!(test)`. Order-independent: it takes effect at the
/// next spawn, even if the gate already exists. `tests/process_admission.rs`
/// fails if any source under `src/` calls it.
#[doc(hidden)]
pub fn run_process_with_unlimited_spawn_rate() {
    PROCESS_UNLIMITED_RATE.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// Set by [`run_process_with_shared_spawn_budget`], which only the shipped
/// binaries call.
static PROCESS_SHARED_BUDGET: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Makes this process draw from the per-user spawn budget every GitPulse
/// process of this user shares (see [`shared_budget`]), as well as its own.
///
/// Opt-in, so that only the processes the budget exists for spend it: the
/// app, `gitpulse-mcp`, `gitpulsed` and `gitpulse-hook` each call it at the
/// top of `main`. Everything else that links this library — every
/// `tests/*.rs` crate and bench, which get the production gate because they
/// build without `cfg(test)` — keeps a per-process budget by construction,
/// rather than spending the running app's and being deferred by it.
/// `tests/process_admission.rs` derives the binary list from `Cargo.toml` and
/// `src/bin/` and fails if one does not make the call. A mobile build would
/// enter through `run()` rather than `main` and stay per-process; none ships.
///
/// The record is opened when the gate is built, by the first spawn, so a
/// call after that could not take effect: it panics rather than leave the
/// process silently off the budget. Absent from the unit-test build, where
/// opting in would open the real record under the developer's `HOME`.
#[cfg(not(test))]
pub fn run_process_with_shared_spawn_budget() {
    assert!(
        SPAWN_GATE.get().is_none(),
        "run_process_with_shared_spawn_budget must run before the first spawn: \
         the spawn gate is already built on a per-process budget"
    );
    PROCESS_SHARED_BUDGET.store(true, std::sync::atomic::Ordering::Relaxed);
}

/// The class a thread starts in: see [`run_process_as_background`].
fn thread_default_admission() -> Admission {
    if PROCESS_BACKGROUND.load(std::sync::atomic::Ordering::Relaxed) {
        Admission::Background
    } else {
        Admission::Reactive
    }
}

thread_local! {
    static ADMISSION: Cell<Admission> = Cell::new(thread_default_admission());
    /// Whether this thread asked for its spawns to be admitted while the disk is
    /// the only thing the host is short of (see [`with_disk_relief`]). The ask
    /// alone admits nothing: the governor must say so too.
    static DISK_RELIEF: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread's spawn is the host-load exec probe (see
    /// [`with_exec_probe`]).
    static EXEC_PROBE: Cell<bool> = const { Cell::new(false) };
    /// Whether this thread's background spawns wait for budget; a thread
    /// starts in the process default, like [`ADMISSION`].
    static AWAITS_BUDGET: Cell<bool> =
        Cell::new(PROCESS_BACKGROUND_AWAITS.load(std::sync::atomic::Ordering::Relaxed));
    /// The repository the guarded mutation running on this thread named, so
    /// the refresh that follows it can be credited (see [`run_command_scope`]).
    static MARKED_REPO: std::cell::RefCell<Option<PathBuf>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
pub(in crate::engine::process) fn marked_repo_is_none() -> bool {
    MARKED_REPO.with(|slot| slot.borrow().is_none())
}

/// The class the current thread's spawns are admitted under. A new thread
/// starts in [`thread_default_admission`]; a body that hops threads carries
/// this value across with [`with_admission`].
pub(crate) fn current_admission() -> Admission {
    ADMISSION.get()
}

/// [`current_admission`] by name, for diagnostics and the process-default
/// contract test, which runs in a process of its own.
pub fn current_admission_name() -> &'static str {
    current_admission().name()
}

/// Runs `body` under `class`, restoring the previous class on return, error
/// and unwinding, so a pooled worker never keeps a class into its next task.
///
/// Entering [`Admission::Background`] from another class also runs the thread
/// at UTILITY QoS for the span (macOS; see `pools::UtilityScope`). The class is
/// the one decision about whether work may be starved, so it drives the
/// thread's scheduling class as well as the children's ([`runs_in_background_band`]).
pub(crate) fn with_admission<T>(class: Admission, body: impl FnOnce() -> T) -> T {
    let previous = ADMISSION.replace(class);
    let _restore = std::mem::DropGuard::new(previous, |prev| {
        ADMISSION.set(prev);
    });
    let _qos = (class == Admission::Background && previous != Admission::Background)
        .then(crate::pools::UtilityScope::enter);
    // User-facing work nested in a background scope gets the thread's own
    // class back for its span, never more than the thread had outside.
    let _user_facing = (class != Admission::Background && previous == Admission::Background)
        .then(crate::pools::UserFacingScope::enter);
    body()
}

/// The source of a spawn no caller named.
pub(in crate::engine::process) const UNATTRIBUTED_SOURCE: &str = "unattributed";

thread_local! {
    /// The call site the current thread's spawns are attributed to in the gate
    /// report. Same shape as [`ADMISSION`]: a forward-only context the runner
    /// reads at admission, which is why it is not threaded through every
    /// runner signature the way the error is (see `CommandError`).
    static SPAWN_SOURCE: Cell<&'static str> = const { Cell::new(UNATTRIBUTED_SOURCE) };
}

/// `git rev-parse` children the process-wide gate has admitted under `source`.
#[cfg(test)]
pub(crate) fn rev_parse_launches_from(source: &str) -> u64 {
    spawn_gate().rev_parse_launches_from(source)
}

/// The call site the current thread's spawns are attributed to. A body that
/// hops threads (`thread::scope`, a pool) must carry it across with
/// [`with_spawn_source`], exactly as it carries [`current_admission`].
pub(in crate::engine::process) fn current_spawn_source() -> &'static str {
    SPAWN_SOURCE.get()
}

/// Runs `body` with its spawns attributed to `source` (a fixed call-site name,
/// never user data), restoring the previous source on return, error and
/// unwinding. The innermost scope wins.
pub(crate) fn with_spawn_source<T>(source: &'static str, body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(SPAWN_SOURCE.replace(source), |prev| {
        SPAWN_SOURCE.set(prev);
    });
    body()
}

#[cfg(test)]
pub(crate) fn with_background_budget_wait<T>(awaits: bool, body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(AWAITS_BUDGET.replace(awaits), |prev| {
        AWAITS_BUDGET.set(prev);
    });
    body()
}

/// Whether this thread's spawn is admitted although the host is shedding: the
/// thread asked for disk relief (see [`with_disk_relief`]) and the governor says
/// the disk remedy is due ([`crate::sysload::disk_remedy_due`]: the disk is the
/// only thing the host is short of, or free disk is below the Severe floor
/// whatever else is pressed, because a nearly full disk brings memory
/// pressure with it). The relief flag is read first, so a thread that never
/// asked costs no governor lock.
fn disk_relief_applies() -> bool {
    if !DISK_RELIEF.get() {
        return false;
    }
    #[cfg(test)]
    if let Some(forced) = DISK_ONLY_FOR_TEST.get() {
        return forced;
    }
    crate::sysload::disk_remedy_due()
}

#[cfg(test)]
thread_local! {
    /// Test seam for the governor's answer: the injected governor has no
    /// history, so it can only answer false. Thread-local, set by
    /// `with_disk_only_answer`.
    static DISK_ONLY_FOR_TEST: Cell<Option<bool>> = const { Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn with_disk_only_answer<T>(answer: bool, body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(DISK_ONLY_FOR_TEST.replace(Some(answer)), |prev| {
        DISK_ONLY_FOR_TEST.set(prev);
    });
    body()
}

/// Runs `body` with the cleaner's bounded, read-only checks admitted while the
/// host sheds work for the disk remedy.
///
/// A full disk is the one pressure the cleaner exists to relieve, and the
/// governor sheds reactive work under it like any other: a run started because
/// the disk is full would then find every check refused and delete nothing.
/// This admits only when `crate::sysload::disk_remedy_due()` holds; that
/// function owns the definition (it is the governor's: the disk is the only
/// reason the host sheds, or free disk is below the Severe floor on a fresh
/// reading whatever else is pressed; it answers false on any doubt: no
/// history for the first branch, an unavailable sensor, no governor in this
/// process, as in the headless run, the MCP server or a hook). Low disk at
/// Elevated, or the fill projection alone, with something else pressed, is not
/// due. It changes nothing for a class that is not shed, and it
/// never makes a spawn cheaper: the slot and rate budgets still apply. The
/// caller must only spawn one read-only check inside it, never a scan: any
/// other spawn on the same thread inside the body would be admitted too.
pub(crate) fn with_disk_relief<T>(body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(DISK_RELIEF.replace(true), |prev| {
        DISK_RELIEF.set(prev);
    });
    body()
}

/// Runs `body` as the host-load exec probe: its one spawn is admitted outside
/// the rate bucket and the load governor's verdict, and only that.
///
/// The probe measures how long a trivial child takes to start, and the
/// governor sheds background work while that is slow. A probe the verdict could
/// shed would stop measuring the moment it was needed and never see start-up
/// recover; one that drew from the git rate bucket would be refused by an
/// empty bucket (the 2026-11-16 shared bucket sat at zero) and read as no
/// signal, or would spend tokens the git reads need. What still bounds it is
/// the concurrency limit (it is `Background` and takes one of the background
/// slots), its own deadline, and the one-in-flight cadence of its caller. Only
/// `sysload::exec_probe` calls it, through [`super::runner::time_exec_probe`].
pub(super) fn with_exec_probe<T>(body: impl FnOnce() -> T) -> T {
    let _restore = std::mem::DropGuard::new(EXEC_PROBE.replace(true), |prev| {
        EXEC_PROBE.set(prev);
    });
    body()
}

/// The load governor's verdict for one spawn of `class`, and the only reader of
/// it: the refusal the gate gives under pressure, or `None` when the class is
/// not shed (a user action never is) or the disk remedy's relief applies.
fn pressure_refusal(class: Admission) -> Option<Refusal> {
    if class == Admission::Interactive || EXEC_PROBE.get() {
        return None;
    }
    let background = class == Admission::Background;
    let shed = crate::sysload::shed();
    let disk_only = disk_relief_applies();
    (!disk_only && ((background && shed.background) || (!background && shed.reactive))).then_some(
        Refusal::Pressure {
            reactive: !background,
            cause: shed.cause,
        },
    )
}

/// Why a background spawn made now, under the disk remedy's relief, would be
/// refused for pressure (the end of a sentence), or `None` when it would not.
/// For a caller that must decide before it starts work: it asks the gate's own
/// judgement, so "the work waits" and "its checks would be refused" cannot
/// disagree.
pub(crate) fn background_deferral_cause() -> Option<&'static str> {
    match with_disk_relief(|| pressure_refusal(Admission::Background)) {
        Some(Refusal::Pressure { cause, .. }) => Some(
            cause
                .unwrap_or(crate::sysload::Binding::MemoryPressure)
                .cause_phrase(),
        ),
        _ => None,
    }
}

/// Applies only inside a synchronous blocking operation.
pub(crate) fn with_background_processes<T>(body: impl FnOnce() -> T) -> T {
    with_admission(Admission::Background, body)
}

/// Marks the rest of the enclosing [`with_admission`] scope as a user action.
/// The command guard calls this once per mutation, and the command runner
/// scopes every command body, so the mark ends with the command that set it.
pub(crate) fn mark_interactive() {
    ADMISSION.set(Admission::Interactive);
}

/// [`mark_interactive`] for a mutation on `repo_path`, which also names the
/// repository whose follow-up refresh [`run_command_scope`] credits.
pub(crate) fn mark_user_action(repo_path: &str) {
    mark_interactive();
    let repo = std::fs::canonicalize(repo_path).unwrap_or_else(|_| PathBuf::from(repo_path));
    MARKED_REPO.with(|slot| *slot.borrow_mut() = Some(repo));
}

/// Runs one IPC command body.
///
/// Every command starts `Reactive`, and the guard promotes the rest of a
/// mutation's body to a user action. When the body ends, the promotion ends
/// with it — the pool thread never carries it into its next task — and two
/// things follow from a body that was a user action:
///
/// * Its error never carries the deferral marker out ([`not_retryable`]). The
///   frontend retries a deferred call on the strength of "nothing ran"; a
///   call that reached the mutation guard may already have changed things,
///   for instance when a thread it hopped to was deferred mid-action.
/// * The repository it named gets a short post-action credit: the refresh the
///   frontend asks for next is admitted like the action that caused it,
///   rather than deferred behind the refresh traffic the action itself set
///   off. See [`POST_ACTION_CREDIT_SPAWNS`].
pub(crate) fn run_command_scope<T>(
    body: impl FnOnce() -> Result<T, CommandError>,
) -> Result<T, CommandError> {
    let _scope = std::mem::DropGuard::new(
        (
            ADMISSION.replace(Admission::Reactive),
            MARKED_REPO.with(|slot| slot.borrow_mut().take()),
        ),
        |(previous, previous_repo)| {
            ADMISSION.set(previous);
            MARKED_REPO.with(|slot| *slot.borrow_mut() = previous_repo);
        },
    );
    let result = body();
    if current_admission() != Admission::Interactive {
        return result;
    }
    if let Some(repo) = MARKED_REPO.with(|slot| slot.borrow_mut().take()) {
        grant_post_action_credit(&repo, Instant::now());
    }
    result.map_err(|error| error.map_message(not_retryable))
}

/// Rewrites a deferral so it no longer reads as one. The cause stays in the
/// sentence; only the marker that means "nothing ran, ask again" goes.
fn not_retryable(message: String) -> String {
    if !is_deferred_under_load(&message) {
        return message;
    }
    message.replacen(
        DEFERRED_MARKER,
        " was deferred under load (the action had already started, so it was not retried) after ",
        1,
    )
}

/// Spawns in the acted-on repository that a user action's follow-up refresh
/// may make without a rate token, and for how long. Sized for one full
/// snapshot (status, branches, tags, stash, operation probe, numstat, default
/// branch probes) with headroom, and short enough that a watcher storm that
/// starts after the action is back under the rate limit within seconds.
/// Inferred from the hydrate read set, not measured per repository.
pub(crate) const POST_ACTION_CREDIT_SPAWNS: u32 = 32;
pub(super) const POST_ACTION_CREDIT_WINDOW: Duration = Duration::from_secs(3);
/// Repositories holding a live credit at once. A user acts on one repository
/// at a time; the bound only stops a scripted burst growing the map.
pub(super) const POST_ACTION_CREDIT_REPOS: usize = 64;

pub(super) struct PostActionCredit {
    pub(super) remaining: u32,
    pub(super) until: Instant,
}

pub(in crate::engine::process) fn post_action_credits(
) -> &'static Mutex<HashMap<PathBuf, PostActionCredit>> {
    static CREDITS: OnceLock<Mutex<HashMap<PathBuf, PostActionCredit>>> = OnceLock::new();
    CREDITS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn grant_post_action_credit_in(
    credits: &mut HashMap<PathBuf, PostActionCredit>,
    repo: &Path,
    now: Instant,
) {
    credits.retain(|_, credit| credit.until > now);
    if credits.len() >= POST_ACTION_CREDIT_REPOS && !credits.contains_key(repo) {
        if let Some(oldest) = credits
            .iter()
            .min_by_key(|(_, credit)| credit.until)
            .map(|(path, _)| path.clone())
        {
            credits.remove(&oldest);
        }
    }
    credits.insert(
        repo.to_path_buf(),
        PostActionCredit {
            remaining: POST_ACTION_CREDIT_SPAWNS,
            until: now + POST_ACTION_CREDIT_WINDOW,
        },
    );
}

pub(super) fn grant_post_action_credit(repo: &Path, now: Instant) {
    let mut credits = post_action_credits()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    grant_post_action_credit_in(&mut credits, repo, now);
}

/// Grants a short interactive credit window for `repo` using the default
/// process gate's clock.
pub(crate) fn grant_post_action_credit_now(repo: &Path) {
    spawn_gate().grant_credit(repo);
}

fn take_post_action_credit_in(
    credits: &mut HashMap<PathBuf, PostActionCredit>,
    cwd: &Path,
    now: Instant,
) -> bool {
    let Some(credit) = credits.get_mut(cwd) else {
        return false;
    };
    if credit.until <= now || credit.remaining == 0 {
        credits.remove(cwd);
        return false;
    }
    credit.remaining -= 1;
    true
}

/// Spends one credit for a spawn whose working directory is `cwd`.
pub(crate) fn take_post_action_credit(cwd: &Path, now: Instant) -> bool {
    let mut credits = post_action_credits()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    take_post_action_credit_in(&mut credits, cwd, now)
}

/// Why the gate started nothing. Each cause is its own variant because each
/// means something different to the person reading it: a deferral is the
/// app protecting itself and says how long it tried; a timeout is a slot that
/// never came free inside the command's own deadline.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Refusal {
    /// A reactive spawn waited [`SPAWN_QUEUE_BUDGET`] for a rate token.
    Refused {
        waited: Duration,
    },
    /// Background work refused without waiting: the rate budget was at or
    /// below the reserve kept for reactive reads.
    Shed,
    /// Refused without waiting because the host is under load (memory, swap or disk: see `cause`) and
    /// this class is shed while it lasts (`sysload::governor`). Carries the
    /// deferral marker, so the frontend retries it once the pressure clears.
    Pressure {
        reactive: bool,
        /// What put the host in the tier that sheds, so the message names it.
        cause: Option<crate::sysload::Binding>,
    },
    /// No concurrency slot became free before the command's own deadline.
    TimedOut {
        deadline: Duration,
    },
    Cancelled,
}

/// What the gate did, per class, since launch. Exported with diagnostics so
/// "user actions failed" can be told apart from "refreshes were deferred".
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct AdmissionCounters {
    pub admitted: u64,
    /// Joined an identical queued read instead of starting a child.
    pub coalesced: u64,
    pub refused: u64,
    pub shed: u64,
    pub timed_out: u64,
    pub cancelled: u64,
}

impl AdmissionCounters {
    fn count(&mut self, refusal: Refusal) {
        match refusal {
            Refusal::Refused { .. } => self.refused += 1,
            Refusal::Shed | Refusal::Pressure { .. } => self.shed += 1,
            Refusal::TimedOut { .. } => self.timed_out += 1,
            Refusal::Cancelled => self.cancelled += 1,
        }
    }
}

/// A counting semaphore over live child processes.
///
/// Deliberately plain `std` rather than a new dependency: the only operations
/// needed are "wait for a slot" and "give one back", and the wait happens on
/// threads that are already blocking on a subprocess.
pub(crate) struct SpawnGate {
    pub(super) limit: usize,
    /// `u32::MAX` turns the token bucket off. The process-wide gate does that
    /// under `cfg(test)` so the suite can spawn git freely; production uses
    /// [`production_spawn_rate`].
    burst: u32,
    refill_per_sec: u32,
    /// The budget every GitPulse process of this user also draws from, when
    /// it could be opened (see [`shared_budget`]). `None` for test gates,
    /// and for a production gate whose record was unusable — `shared_note`
    /// then says why, so a per-process budget is never reported as shared.
    pub(super) shared: Option<shared_budget::SharedBudget>,
    pub(super) shared_note: Option<String>,
    pub(super) state: Mutex<GateState>,
    pub(super) released: Condvar,
    pub(super) clock: Arc<dyn Clock>,
    clock_epoch: Instant,
    wall_epoch_ns: u64,
    use_clock_for_shared: bool,
    refusal_report_at: Mutex<Option<Instant>>,
    pub(super) shared_fallback_report_at: Mutex<Option<Instant>>,
    pub(super) unrecorded_denial_report_at: Mutex<Option<Instant>>,
    local_credits: Option<Mutex<HashMap<PathBuf, PostActionCredit>>>,
}

#[derive(Default)]
pub(super) struct GateState {
    pub(super) in_flight: usize,
    pub(super) background: usize,
    pub(super) waiting_background: usize,
    /// Interactive callers waiting for a slot. While any wait, the next free
    /// slot is theirs: a user action never queues behind a refresh.
    pub(super) waiting_interactive: usize,
    pub(super) counters: [AdmissionCounters; 3],
    /// High-water mark, kept so a test can assert the ceiling actually held
    /// rather than assert on the counter it is trying to prove bounded.
    pub(super) peak: usize,
    /// Whole tokens remaining. Integer so a fractional refill cannot admit an
    /// extra child, and so concurrent waiters cannot share one token via float
    /// rounding. The mutex makes the decrement atomic with the check.
    pub(super) tokens: u32,
    pub(super) tokens_at: Option<Instant>,
    /// Reactive waiters that have not yet been granted a shared rate token, by
    /// arrival ticket, oldest first. Only the front may ask the shared budget:
    /// without an order the waiters race for each token as it lands, so a
    /// read that waited a whole deadline loses it to one that arrived a
    /// moment ago and is refused while later arrivals are served. Reactive
    /// only, on purpose: an interactive caller never waits for a token, a
    /// background one is shed or held to a reserve that a reactive read is
    /// not, so putting either in this line would let a head blocked on its own
    /// floor stall every read behind it. Every reactive waiter is blocked by
    /// the same slot, class and local-token conditions, so a front that
    /// cannot ask means none behind it can either.
    pub(super) reactive_line: VecDeque<u64>,
    next_ticket: u64,
    /// Decisions that could not consult the shared budget and fell back to
    /// this process's own (a lock held past its patience, an I/O error).
    pub(super) shared_fallbacks: u64,
    /// Decisions the shared budget denied but could not record. Refused like
    /// any denial (and so also in `shed` / `deferred`), counted here as well so
    /// a record that has stopped taking writes shows in the report instead of
    /// only in a log line that is throttled.
    pub(super) shared_unrecorded_denials: u64,
    /// Admitted children by what they were (`git status`, `gh`, ...). The
    /// class totals said the budget was spent; this says on what, which is
    /// the question every refusal raised and no log could answer.
    pub(super) by_label: LabelCounts,
}

/// Admissions per spawn label, bounded so a stream of novel labels cannot
/// grow it: past [`LabelCounts::MAX_LABELS`] distinct labels, the rest are
/// counted under [`LabelCounts::OTHER`].
#[derive(Default)]
pub(super) struct LabelCounts {
    pub(super) counts: HashMap<String, u64>,
    /// `git rev-parse` admissions by the call site that asked
    /// ([`with_spawn_source`]), bounded like `counts`. Sources are `&'static
    /// str` names chosen in code, never paths or ref names.
    pub(super) rev_parse_sources: HashMap<&'static str, u64>,
}

impl LabelCounts {
    pub(super) const MAX_LABELS: usize = 64;
    const OTHER: &'static str = "other";
    /// Labels printed per report; the rest are summed into one figure.
    pub(super) const REPORTED: usize = 8;
    /// Distinct rev-parse sources kept; the rest count under [`Self::OTHER`].
    pub(super) const MAX_SOURCES: usize = 32;
    /// Sources printed per report; the rest are summed into `rest=`.
    const REPORTED_SOURCES: usize = 5;
    /// The label (as [`report_label`] reduces it) whose launches are
    /// attributed to a source.
    const SOURCED_LABEL: &'static str = "git rev-parse";

    /// [`Self::note`], and for a `git rev-parse` also the call site behind it.
    pub(super) fn note_sourced(&mut self, label: &str, source: &'static str) {
        self.note(label);
        if report_label(label) != Self::SOURCED_LABEL {
            return;
        }
        let source = if !self.rev_parse_sources.contains_key(source)
            && self.rev_parse_sources.len() >= Self::MAX_SOURCES
        {
            Self::OTHER
        } else {
            source
        };
        *self.rev_parse_sources.entry(source).or_insert(0) += 1;
    }

    /// `rev-parse by: a.b=9 c=4 unattributed=2 rest=1`, largest first, ties by
    /// name; empty when no rev-parse was admitted.
    pub(super) fn sources_report(&self) -> String {
        if self.rev_parse_sources.is_empty() {
            return String::new();
        }
        let mut rows: Vec<(&&str, &u64)> = self.rev_parse_sources.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let shown: Vec<String> = rows
            .iter()
            .take(Self::REPORTED_SOURCES)
            .map(|(source, count)| format!("{source}={count}"))
            .collect();
        let rest: u64 = rows
            .iter()
            .skip(Self::REPORTED_SOURCES)
            .map(|(_, count)| **count)
            .sum();
        if rest == 0 {
            format!("rev-parse by: {}", shown.join(" "))
        } else {
            format!("rev-parse by: {} rest={rest}", shown.join(" "))
        }
    }

    pub(super) fn note(&mut self, label: &str) {
        let key = report_label(label);
        if let Some(count) = self.counts.get_mut(&key) {
            *count += 1;
            return;
        }
        let key = if self.counts.len() >= Self::MAX_LABELS {
            Self::OTHER.to_string()
        } else {
            key
        };
        *self.counts.entry(key).or_insert(0) += 1;
    }

    /// `top: git status=310 git for-each-ref=220 ... rest=41`, largest first,
    /// ties by name so the line is stable.
    pub(super) fn report(&self) -> String {
        if self.counts.is_empty() {
            return "top: none".to_string();
        }
        let mut rows: Vec<(&String, &u64)> = self.counts.iter().collect();
        rows.sort_by(|a, b| b.1.cmp(a.1).then_with(|| a.0.cmp(b.0)));
        let shown: Vec<String> = rows
            .iter()
            .take(Self::REPORTED)
            .map(|(label, count)| format!("{label}={count}"))
            .collect();
        let rest: u64 = rows
            .iter()
            .skip(Self::REPORTED)
            .map(|(_, count)| **count)
            .sum();
        if rest == 0 {
            format!("top: {}", shown.join(" "))
        } else {
            format!("top: {} rest={rest}", shown.join(" "))
        }
    }
}

/// A spawn label reduced to what may be written to a diagnostics log: its
/// first two words, each kept only if it looks like a program or subcommand
/// name. A label is caller-built and could carry a path or a ref name; those
/// become `…`, so attribution can never leak repository contents.
pub(super) fn report_label(label: &str) -> String {
    let words: Vec<&str> = label
        .split_whitespace()
        .take(2)
        .map(|word| {
            let plain = !word.is_empty()
                && word.len() <= 32
                && word
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
                && !word.starts_with('.');
            if plain {
                word
            } else {
                "…"
            }
        })
        .collect();
    if words.is_empty() {
        "…".to_string()
    } else {
        words.join(" ")
    }
}

impl SpawnGate {
    pub(crate) fn new(limit: usize) -> Self {
        Self::with_rate(limit, u32::MAX, u32::MAX)
    }

    pub(crate) fn with_rate(limit: usize, burst: u32, refill_per_sec: u32) -> Self {
        Self::build(limit, burst, refill_per_sec, Arc::new(SystemClock), false)
    }

    #[cfg(test)]
    pub(super) fn with_clock(
        limit: usize,
        burst: u32,
        refill_per_sec: u32,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::build(limit, burst, refill_per_sec, clock, true)
    }

    fn build(
        limit: usize,
        burst: u32,
        refill_per_sec: u32,
        clock: Arc<dyn Clock>,
        use_clock_for_shared: bool,
    ) -> Self {
        let unlimited = refill_per_sec == u32::MAX;
        // A zero rate would wait out every deadline and look like a hung git.
        // Unlimited (`u32::MAX`) is the test-suite gate and must stay unlimited.
        let burst = if unlimited { burst } else { burst.max(1) };
        let refill_per_sec = if unlimited {
            refill_per_sec
        } else {
            refill_per_sec.max(1)
        };
        let clock_epoch = clock.now();
        let wall_epoch_ns = if use_clock_for_shared {
            shared_budget::wall_ns()
        } else {
            0
        };
        Self {
            limit: limit.max(1),
            burst,
            refill_per_sec,
            shared: None,
            shared_note: None,
            state: Mutex::new(GateState {
                tokens: if unlimited { 0 } else { burst },
                ..GateState::default()
            }),
            released: Condvar::new(),
            clock,
            clock_epoch,
            wall_epoch_ns,
            use_clock_for_shared,
            refusal_report_at: Mutex::new(None),
            shared_fallback_report_at: Mutex::new(None),
            unrecorded_denial_report_at: Mutex::new(None),
            local_credits: use_clock_for_shared.then(|| Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    pub(super) fn grant_credit(&self, repo: &Path) {
        let now = self.clock.now();
        match &self.local_credits {
            Some(local) => {
                let mut credits = local
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                grant_post_action_credit_in(&mut credits, repo, now);
            }
            None => grant_post_action_credit(repo, now),
        }
    }

    #[cfg(not(test))]
    pub(crate) fn grant_credit(&self, repo: &Path) {
        grant_post_action_credit(repo, self.clock.now());
    }

    pub(super) fn take_credit(&self, cwd: &Path) -> bool {
        let now = self.clock.now();
        match &self.local_credits {
            Some(local) => {
                let mut credits = local
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                take_post_action_credit_in(&mut credits, cwd, now)
            }
            None => take_post_action_credit(cwd, now),
        }
    }

    /// Also draws every rate token from the record at `path`, shared with
    /// every other process that opens it. An unusable record leaves the gate
    /// on its own bucket and keeps the reason, so the report says so.
    pub(super) fn sharing(mut self, path: Result<PathBuf, String>) -> Self {
        if self.refill_per_sec == u32::MAX {
            self.shared_note = Some("unlimited gate: no rate budget to share".into());
            return self;
        }
        match path.and_then(|path| shared_budget::SharedBudget::open(&path)) {
            Ok(shared) => {
                self.shared_note = None;
                self.shared = Some(shared);
            }
            Err(reason) => {
                // Not opting in is the intended state of every process that
                // is not a shipped binary, not a fault worth a warning.
                if reason != SHARED_BUDGET_NOT_OPTED_IN {
                    log::warn!(
                        target: "spawn_gate",
                        "spawn rate budget is per-process only: {reason}"
                    );
                }
                self.shared_note = Some(reason);
                self.shared = None;
            }
        }
        self
    }

    /// Asks the shared budget for the token a non-interactive spawn is about
    /// to spend. Does file IO and may wait on a lock another process holds, so
    /// it must be called with the gate mutex released; the answer is applied
    /// by [`Self::settle_shared`].
    fn ask_shared(&self, class: Admission) -> (shared_budget::Take, Option<u32>) {
        let Some(shared) = &self.shared else {
            return (shared_budget::Take::Granted, None);
        };
        if self.use_clock_for_shared {
            let elapsed_ns = u64::try_from(
                self.clock
                    .now()
                    .saturating_duration_since(self.clock_epoch)
                    .as_nanos(),
            )
            .unwrap_or(u64::MAX);
            shared.take_at_leveled(
                self.floor_for(class),
                self.burst,
                self.refill_per_sec,
                self.wall_epoch_ns.saturating_add(elapsed_ns),
            )
        } else {
            shared.take_leveled(self.floor_for(class), self.burst, self.refill_per_sec)
        }
    }

    /// The tokens a spawn of `class` may not spend: none for reactive work,
    /// the reserve for background work.
    fn floor_for(&self, class: Admission) -> u32 {
        if class == Admission::Background {
            self.background_reserve()
        } else {
            0
        }
    }

    fn level_of(&self, bucket: RefusingBucket, tokens: u32, class: Admission) -> BucketLevel {
        BucketLevel {
            bucket,
            tokens,
            floor: self.floor_for(class),
            burst: self.burst,
        }
    }

    /// Applies [`Self::ask_shared`]'s answer under the gate lock: `true` when
    /// the token was granted or the record could not be consulted (counted,
    /// and logged at most every [`REFUSAL_REPORT_INTERVAL`]); `false` when the
    /// processes together have spent it, whether or not the record could be
    /// rewritten to say so (counted apart, in `shared_unrecorded_denials`).
    pub(super) fn settle_shared(&self, state: &mut GateState, taken: shared_budget::Take) -> bool {
        match taken {
            shared_budget::Take::Granted => true,
            shared_budget::Take::Denied => false,
            shared_budget::Take::DeniedUnrecorded(reason) => {
                state.shared_unrecorded_denials = state.shared_unrecorded_denials.saturating_add(1);
                self.log_shared_warning(
                    &self.unrecorded_denial_report_at,
                    "shared spawn budget denied a decision it could not record",
                    "refused",
                    &reason,
                    state.shared_unrecorded_denials,
                );
                false
            }
            shared_budget::Take::Unavailable(reason) => {
                state.shared_fallbacks = state.shared_fallbacks.saturating_add(1);
                self.log_shared_warning(
                    &self.shared_fallback_report_at,
                    "shared spawn budget unavailable for a decision",
                    "used this process's own",
                    &reason,
                    state.shared_fallbacks,
                );
                true
            }
        }
    }

    /// One line on where this gate's rate budget lives, for the report.
    pub(super) fn sharing_report(&self, fallbacks: u64, unrecorded_denials: u64) -> String {
        match (&self.shared, &self.shared_note) {
            (Some(shared), _) => format!(
                "budget=shared({}) unrecorded_denials={unrecorded_denials} fallbacks={fallbacks}",
                shared.path().display()
            ),
            (None, Some(note)) => format!("budget=per-process ({note})"),
            (None, None) => "budget=per-process".to_string(),
        }
    }

    pub(super) fn refill(&self, state: &mut GateState, now: Instant) {
        if self.refill_per_sec == u32::MAX {
            return;
        }
        let Some(at) = state.tokens_at else {
            state.tokens_at = Some(now);
            return;
        };
        let elapsed_ms = now.saturating_duration_since(at).as_millis();
        if elapsed_ms == 0 {
            return;
        }
        // Keep the unused remainder on `tokens_at` so 249 ms at 4/s does not
        // become a token, and the next 1 ms does.
        let add = elapsed_ms.saturating_mul(u128::from(self.refill_per_sec)) / 1000;
        if add == 0 {
            return;
        }
        let add_u = u32::try_from(add).unwrap_or(u32::MAX);
        state.tokens = state.tokens.saturating_add(add_u).min(self.burst);
        let consumed_ms = add.saturating_mul(1000) / u128::from(self.refill_per_sec);
        let consumed = Duration::from_millis(u64::try_from(consumed_ms).unwrap_or(u64::MAX));
        state.tokens_at = Some(at.checked_add(consumed).unwrap_or(now));
    }

    #[cfg(test)]
    pub(super) fn acquire(&self, deadline: Instant) -> Option<SpawnPermit<'_>> {
        self.acquire_until(deadline, &|| false).ok()
    }

    /// [`Self::acquire_judged`] without the level of the bucket that refused.
    #[cfg(test)]
    pub(super) fn acquire_until(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SpawnPermit<'_>, Refusal> {
        self.acquire_judged(deadline, cancelled)
            .map_err(|(refusal, _)| refusal)
    }

    /// Tokens background work may not spend: a quarter of the burst, so
    /// optional work is shed while reactive reads still have budget.
    /// How long one token takes to refill: the soonest the shared record can
    /// answer differently than it just did.
    fn refill_interval(&self) -> Duration {
        Duration::from_millis(1000_u64.div_ceil(u64::from(self.refill_per_sec.max(1))))
    }

    pub(super) fn background_reserve(&self) -> u32 {
        self.burst / 4
    }

    /// Waits for a slot and a rate token until `deadline`. A refusal that was
    /// the rate budget's carries the bucket that refused and its level, for
    /// [`refusal_message_at`].
    pub(super) fn acquire_judged(
        &self,
        deadline: Instant,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<SpawnPermit<'_>, (Refusal, Option<BucketLevel>)> {
        let class = current_admission();
        let background = class == Admission::Background;
        let interactive = class == Admission::Interactive;
        let awaits_budget = background && AWAITS_BUDGET.get();
        // The load governor's verdict, one owner (`sysload::governor`): under
        // memory pressure these classes are refused at once with the
        // deferral marker. A user action is never shed, and neither is a read
        // credited to one (`take_post_action_credit` promoted it already).
        if let Some(refusal) = pressure_refusal(class) {
            self.state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .counters[class.index()]
            .count(refusal);
            return Err((refusal, None));
        }
        let rate_limited = self.refill_per_sec != u32::MAX
            && !PROCESS_UNLIMITED_RATE.load(std::sync::atomic::Ordering::Relaxed)
            && !EXEC_PROBE.get();
        let entered = self.clock.now();
        // Declared before the state guard so that, unwinding, the guard is
        // released first and the reservation can take the lock to give its
        // waiting count back. A stranded interactive or background count
        // would make every later refresh ineligible for good.
        let mut reservation = WaitReservation {
            gate: self,
            class,
            held: false,
            ticket: None,
        };
        // A poisoned gate must not deadlock the app: a panic inside a permit
        // holder still ran the `Drop` below, so the count is accurate.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if background {
            state.waiting_background += 1;
            self.released.notify_all();
        }
        if interactive {
            state.waiting_interactive += 1;
        }
        reservation.held = background || interactive;
        // A reactive read that may have to wait for a shared token takes its
        // place in line on arrival, so a later arrival cannot take a token
        // from under it (see `GateState::reactive_line`).
        if class == Admission::Reactive && rate_limited && self.shared.is_some() {
            let ticket = state.next_ticket;
            state.next_ticket = ticket.wrapping_add(1);
            state.reactive_line.push_back(ticket);
            reservation.ticket = Some(ticket);
        }
        let mut token_wait_started: Option<Instant> = None;
        // When this waiter may next ask the shared record. A record changes at
        // most once per refill interval, so a waiter that was denied has
        // nothing to learn from asking sooner, and each ask is a locked read
        // and rewrite of a file every process shares.
        let mut next_shared_ask: Option<Instant> = None;
        // A token the shared budget granted while the gate lock was released.
        // Kept for the next pass, which re-reads the gate before spending it:
        // the slot it was asked for may have gone to another waiter meanwhile.
        // One that is never spent (the waiter is cancelled or times out first)
        // is lost to the shared budget, which has no way to take it back.
        let mut shared_token_held = false;
        // What the shared record held the last time it denied this waiter, and
        // which bucket was short the last time this waiter was refused a
        // token: what a refusal names.
        let mut shared_denial: Option<BucketLevel> = None;
        let mut short: Option<BucketLevel> = None;
        let outcome = loop {
            // The cancel check is the caller's code. Under the gate lock, a
            // check that took a lock of its own or spawned anything would
            // deadlock the whole gate, so it runs with the lock released and
            // everything below is recomputed from fresh state.
            drop(state);
            let is_cancelled = cancelled();
            state = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let now = self.clock.now();
            let remaining = deadline.saturating_duration_since(now);
            if is_cancelled {
                break Err((Refusal::Cancelled, None));
            }
            if remaining.is_zero() {
                // Whatever the caller was last waiting for is the cause. A
                // short deadline that ran out while the rate budget was spent
                // is a deferral, not a slot that never came free.
                break Err(match token_wait_started {
                    Some(_) => (
                        Refusal::Refused {
                            waited: now.saturating_duration_since(entered),
                        },
                        short,
                    ),
                    None => (
                        Refusal::TimedOut {
                            deadline: deadline.saturating_duration_since(entered),
                        },
                        None,
                    ),
                });
            }
            // Optional work gets at most a quarter of the slots (at least
            // one). Conversely, a waiting background class gets the next
            // available slot if no background child is running, so sustained
            // reactive traffic cannot indefinitely postpone all indexing. A
            // waiting user action outranks both.
            let eligibility = |state: &GateState| match class {
                Admission::Interactive => true,
                Admission::Background => {
                    state.waiting_interactive == 0 && state.background < (self.limit / 4).max(1)
                }
                Admission::Reactive => {
                    state.waiting_interactive == 0
                        && (state.waiting_background == 0
                            || state.background > 0
                            || state.in_flight < self.limit.saturating_sub(1))
                }
            };
            let tokens_available = |state: &GateState| match class {
                Admission::Interactive => true,
                Admission::Reactive => !rate_limited || state.tokens >= 1,
                Admission::Background => !rate_limited || state.tokens > self.background_reserve(),
            };
            let mut eligible = eligibility(&state);
            self.refill(&mut state, now);
            let mut tokens_ok = tokens_available(&state);
            // A reactive read that is not at the front of the line does not
            // ask: the token that lands next is the oldest waiter's. It naps
            // like any waiter short of a token, and is woken when the front
            // changes.
            if reservation
                .ticket
                .is_some_and(|ticket| state.reactive_line.front() != Some(&ticket))
            {
                tokens_ok = false;
            }
            // The shared budget is asked last, and only by a spawn that would
            // otherwise start now, so a granted token is spent at once below
            // and never taken for a waiter that then gives up.
            let asks_shared = tokens_ok
                && rate_limited
                && !interactive
                && !shared_token_held
                && state.in_flight < self.limit
                && eligible
                && self.shared.is_some();
            // A waiter (anything that will not be refused at once) is paced to
            // the refill interval; a first ask, or a spawn that is refused on
            // its answer, is not delayed.
            let paced = asks_shared
                && (!background || awaits_budget)
                && next_shared_ask.is_some_and(|at| now < at);
            if paced {
                tokens_ok = false;
            } else if asks_shared {
                // The shared record is a file under a lock that may sleep-poll
                // for [`shared_budget::LOCK_PATIENCE`], and a denial writes it
                // too. None of that may happen under the gate mutex, where it
                // would queue every other acquire, a user action included,
                // behind a background re-poll. Released like the cancel check
                // above, and everything is re-read once the answer is in.
                drop(state);
                let (taken, shared_level) = self.ask_shared(class);
                state = self
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if self.settle_shared(&mut state, taken) {
                    shared_token_held = true;
                    // Served: the next in line is now the oldest waiter.
                    reservation.leave_line(&mut state);
                    // The lock was released: another waiter may have taken the
                    // slot or the local token, so both are read again. The
                    // deadline is not: a token already granted is spent, not
                    // thrown away because the wait for it ran long.
                    eligible = eligibility(&state);
                    tokens_ok = tokens_available(&state);
                } else {
                    tokens_ok = false;
                    next_shared_ask = Some(self.clock.now() + self.refill_interval());
                    shared_denial = shared_level
                        .map(|tokens| self.level_of(RefusingBucket::Shared, tokens, class));
                }
            }
            // An awaiting background reader counts in `waiting_background`
            // only while it waits for a SLOT, which is what the count reserves
            // a slot for (see `eligibility`). One short of tokens cannot use a
            // slot, so it must not keep one from a reactive read that can.
            if awaits_budget {
                if reservation.held && !tokens_ok {
                    state.waiting_background = state.waiting_background.saturating_sub(1);
                    reservation.held = false;
                    self.released.notify_all();
                } else if !reservation.held && tokens_ok {
                    state.waiting_background += 1;
                    reservation.held = true;
                }
            }
            if !tokens_ok {
                // The process bucket is short first; past it, the record is.
                short = if tokens_available(&state) {
                    shared_denial
                } else {
                    Some(self.level_of(RefusingBucket::Local, state.tokens, class))
                };
                // Background is shed at once: it is the first thing to go
                // under load, and waiting would only hold a pool thread.
                // Unless this process asked to wait (an agent's MCP read: see
                // [`run_process_awaiting_background_budget`]), which then
                // waits like a reactive read, still above the reserve.
                if background && !awaits_budget {
                    break Err((Refusal::Shed, short));
                }
                let started = *token_wait_started.get_or_insert(now);
                let waited = now.saturating_duration_since(started);
                // A storm that has spent the burst must fail the spawn instead
                // of occupying a blocking-pool thread for the whole git timeout.
                if waited >= SPAWN_QUEUE_BUDGET {
                    break Err((
                        Refusal::Refused {
                            waited: now.saturating_duration_since(entered),
                        },
                        short,
                    ));
                }
            } else {
                token_wait_started = None;
            }
            if state.in_flight < self.limit && eligible && tokens_ok {
                break Ok(());
            }
            // Wake when the next token is due rather than on a fixed poll: a
            // waiter refused a token has nothing to learn before then. The
            // wait is still capped at the cancellation poll, and at the
            // deadline.
            // The shared ask time only says when to wake while it is still in
            // the future; once past, whichever bucket is refusing now (the
            // local one, say) refills on the same interval.
            let nap = match (tokens_ok, next_shared_ask) {
                (false, Some(at)) if at > now => at.saturating_duration_since(now),
                (false, _) if rate_limited => self.refill_interval(),
                _ => CANCEL_POLL,
            }
            .max(MIN_TOKEN_NAP);
            state = super::clock::wait_timeout(
                self.clock.as_ref(),
                &self.state,
                &self.released,
                state,
                remaining.min(nap.min(CANCEL_POLL)),
            );
        };
        reservation.give_back(&mut state);
        if let Err((refusal, level)) = outcome {
            state.counters[class.index()].count(refusal);
            let counters = state.counters;
            let sharing =
                self.sharing_report(state.shared_fallbacks, state.shared_unrecorded_denials);
            drop(state);
            // A refused waiter must release its reservation.
            self.released.notify_all();
            if refusal != Refusal::Cancelled {
                // Rendered only when the throttle lets a line out: refusals
                // come in storms, and sorting the labels under the gate lock
                // for every one would tax exactly the path being diagnosed.
                self.log_refusals(&counters, &sharing, level, || self.label_report());
            }
            return Err((refusal, level));
        }
        state.in_flight += 1;
        if background {
            state.background += 1;
        }
        if rate_limited && !interactive {
            state.tokens = state.tokens.saturating_sub(1);
        }
        state.counters[class.index()].admitted += 1;
        state.peak = state.peak.max(state.in_flight);
        Ok(SpawnPermit {
            gate: self,
            background,
        })
    }

    /// The `top: ...` attribution line, rendered under a brief lock.
    pub(super) fn label_report(&self) -> String {
        let state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let top = state.by_label.report();
        let sources = state.by_label.sources_report();
        if sources.is_empty() {
            top
        } else {
            format!("{top} | {sources}")
        }
    }

    /// Attributes one admitted child to its label, and its rev-parse launches
    /// to the call site on this thread ([`current_spawn_source`]), for the gate
    /// report.
    pub(super) fn note_admitted_label(&self, label: &str) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_label
            .note_sourced(label, current_spawn_source());
    }

    pub(super) fn note_coalesced(&self, class: Admission) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counters[class.index()]
        .coalesced += 1;
    }

    /// Takes back a [`Self::note_coalesced`] for a joiner whose leader timed
    /// out and who asks again: it was not answered by another's child.
    pub(super) fn note_uncoalesced(&self, class: Admission) {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let counters = &mut state.counters[class.index()];
        counters.coalesced = counters.coalesced.saturating_sub(1);
    }

    #[cfg(test)]
    fn rev_parse_launches_from(&self, source: &str) -> u64 {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .by_label
            .rev_parse_sources
            .get(source)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(super) fn counters(&self) -> [AdmissionCounters; 3] {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .counters
    }

    #[cfg(test)]
    pub(super) fn peak(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .peak
    }
}

/// A waiter's place in `waiting_interactive` / `waiting_background`. Given
/// back under the caller's lock on every normal exit, and by `Drop` when the
/// waiter unwinds, so a panic anywhere in the wait cannot strand it.
struct WaitReservation<'a> {
    gate: &'a SpawnGate,
    class: Admission,
    held: bool,
    /// This waiter's place in `GateState::reactive_line`, if it has one.
    ticket: Option<u64>,
}

impl WaitReservation<'_> {
    /// Leaves the reactive line, if in it, and wakes the waiters so the new
    /// front asks at once. Every exit calls this (normal or by unwinding), so
    /// a stranded front can never hold the line.
    fn leave_line(&mut self, state: &mut GateState) {
        let Some(ticket) = self.ticket.take() else {
            return;
        };
        if let Some(at) = state.reactive_line.iter().position(|t| *t == ticket) {
            state.reactive_line.remove(at);
        }
        self.gate.released.notify_all();
    }

    fn give_back(&mut self, state: &mut GateState) {
        self.leave_line(state);
        if !std::mem::take(&mut self.held) {
            return;
        }
        match self.class {
            Admission::Interactive => {
                state.waiting_interactive = state.waiting_interactive.saturating_sub(1)
            }
            Admission::Background => {
                state.waiting_background = state.waiting_background.saturating_sub(1)
            }
            Admission::Reactive => {}
        }
    }
}

impl Drop for WaitReservation<'_> {
    fn drop(&mut self) {
        if !self.held && self.ticket.is_none() {
            return;
        }
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        self.give_back(&mut state);
        drop(state);
        self.gate.released.notify_all();
    }
}

/// Releases its slot on drop, so an early `?` return, a timeout path and a
/// panic all give the descriptor budget back.
pub(super) struct SpawnPermit<'a> {
    gate: &'a SpawnGate,
    background: bool,
}

impl Drop for SpawnPermit<'_> {
    fn drop(&mut self) {
        let mut state = self
            .gate
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        state.in_flight = state.in_flight.saturating_sub(1);
        if self.background {
            state.background = state.background.saturating_sub(1);
        }
        drop(state);
        // Waking only an ineligible class can strand an otherwise free slot.
        self.gate.released.notify_all();
    }
}

/// Concurrent children allowed: twice the core count, clamped so a 2-core CI
/// box still overlaps work and a 32-core workstation does not put 64 `git`
/// processes and ~2 GiB of potential drain buffers on the host at once.
pub(super) fn spawn_limit() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get().saturating_mul(2))
        .unwrap_or(SPAWN_LIMIT_FLOOR)
        .clamp(SPAWN_LIMIT_FLOOR, SPAWN_LIMIT_CEILING)
}

/// Sustained git spawns per second, and the burst that may start immediately.
///
/// The measured storm on 2026-10-02 was 40–80 short-lived `git` processes per
/// second for hours. Concurrency alone does not stop that: 16 children that
/// each live ~200 ms still start about 80 processes a second. The burst covers
/// one uncached branch-stat pass (`MAX_BRANCH_STAT_TARGETS` × 2); after that
/// the refill is the hard cap.
const SPAWN_BURST: u32 = 192;
const SPAWN_PER_SEC: u32 = 8;
/// How long a caller may block once the burst is spent. Longer than one
/// refill interval, short enough that a saturated storm does not pin the
/// blocking pool until the 90s git deadline.
pub(crate) const SPAWN_QUEUE_BUDGET: Duration = Duration::from_secs(2);

/// The longest a waiter sleeps before it re-checks for cancellation.
const CANCEL_POLL: Duration = Duration::from_millis(50);
/// The shortest a waiter naps for a token that is due, so one already due
/// cannot spin the loop.
const MIN_TOKEN_NAP: Duration = Duration::from_millis(1);

pub(crate) fn production_spawn_rate() -> (u32, u32) {
    (SPAWN_BURST, SPAWN_PER_SEC)
}

pub(super) fn configured_spawn_gate(testing: bool) -> SpawnGate {
    if testing {
        SpawnGate::new(spawn_limit())
    } else {
        let (burst, rate) = production_spawn_rate();
        SpawnGate::with_rate(spawn_limit(), burst, rate).sharing(shared_budget_path())
    }
}

/// Why a production gate in a process that did not opt in stays per-process.
const SHARED_BUDGET_NOT_OPTED_IN: &str = "this process did not opt into the per-user spawn budget";

/// The per-user record, for a process that opted in (see
/// [`run_process_with_shared_spawn_budget`]). Every other process — the
/// unit-test build, integration tests, benches — never touches the real one:
/// a test that spends a burst would otherwise spend the running app's.
fn shared_budget_path() -> Result<PathBuf, String> {
    if PROCESS_SHARED_BUDGET.load(std::sync::atomic::Ordering::Relaxed) {
        shared_budget::default_path()
    } else {
        Err(SHARED_BUDGET_NOT_OPTED_IN.into())
    }
}

impl SpawnGate {
    /// Writes the totals into the diagnostic log when the gate refuses work, at
    /// most every [`REFUSAL_REPORT_INTERVAL`], so an exported log says whether a
    /// failed action was a deferred refresh, shed background work or a timeout,
    /// and how much was admitted meanwhile. A real log entry rather than a line
    /// appended at export: the export's "empty log" verdict must stay true.
    fn log_refusals(
        &self,
        counters: &[AdmissionCounters; 3],
        sharing: &str,
        refused_by: Option<BucketLevel>,
        top: impl FnOnce() -> String,
    ) {
        if !self.report_due_at(&self.refusal_report_at, self.clock.now()) {
            return;
        }
        let refused_by = refused_by.map_or_else(
            || "last refusal named no bucket".to_string(),
            |level| format!("last refusal: {}", level.describe()),
        );
        log::warn!(
            target: "spawn_gate",
            "{} | {refused_by} | {} | {}",
            format_gate_report(counters, sharing),
            top(),
            crate::watcher::skip::summary()
        );
    }

    /// Logs a throttled shared-budget diagnostic at most once per
    /// [`REFUSAL_REPORT_INTERVAL`] on this gate's clock.
    fn log_shared_warning(
        &self,
        last: &Mutex<Option<Instant>>,
        summary: &str,
        action: &str,
        reason: &str,
        total: u64,
    ) {
        if !self.report_due_at(last, self.clock.now()) {
            return;
        }
        log::warn!(
            target: "spawn_gate",
            "{summary} ({total} so far); {action}: {reason}"
        );
    }

    /// Whether `last` is older than [`REFUSAL_REPORT_INTERVAL`] relative to
    /// `now`; if so, `now` is recorded as the latest report.
    pub(super) fn report_due_at(&self, last: &Mutex<Option<Instant>>, now: Instant) -> bool {
        let mut last = last
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if last.is_some_and(|at| now.saturating_duration_since(at) < REFUSAL_REPORT_INTERVAL) {
            return false;
        }
        *last = Some(now);
        true
    }
}

pub(super) const REFUSAL_REPORT_INTERVAL: Duration = Duration::from_secs(30);

pub(super) fn format_gate_report(counters: &[AdmissionCounters; 3], sharing: &str) -> String {
    let classes: Vec<String> = Admission::ALL
        .iter()
        .map(|class| {
            let c = counters[class.index()];
            format!(
                "{} admitted={} coalesced={} deferred={} shed={} timed_out={} cancelled={}",
                class.name(),
                c.admitted,
                c.coalesced,
                c.refused,
                c.shed,
                c.timed_out,
                c.cancelled
            )
        })
        .collect();
    format!("[spawn-gate] {} | {sharing}", classes.join(" | "))
}

/// The process-wide gate, built by the first spawn. Module-level so that
/// [`run_process_with_shared_spawn_budget`] can refuse to run after it.
static SPAWN_GATE: OnceLock<SpawnGate> = OnceLock::new();

pub(super) fn spawn_gate() -> &'static SpawnGate {
    // Tests share this process-wide gate and spawn far above the storm cap.
    // Production uses the same constructor with `testing == false`.
    SPAWN_GATE.get_or_init(|| configured_spawn_gate(cfg!(test)))
}

#[cfg(test)]
pub(crate) fn leak_gate(gate: SpawnGate) -> &'static SpawnGate {
    Box::leak(Box::new(gate))
}

/// A gate whose bucket cannot refill during the test. The rate is real time,
/// so on a loaded machine elapsed seconds could otherwise mint a token mid-test;
/// putting `tokens_at` in the future freezes elapsed time at zero.
#[cfg(test)]
pub(crate) fn frozen_gate(gate: SpawnGate) -> &'static SpawnGate {
    let gate = leak_gate(gate);
    let future = gate.clock.now() + Duration::from_secs(86_400);
    gate.state.lock().unwrap().tokens_at = Some(future);
    gate
}

/// Keeps `repo`'s post-action credit open for the test across slow spawns.
#[cfg(unix)]
#[cfg(test)]
pub(crate) fn hold_credit_open(repo: &Path) {
    let mut credits = post_action_credits().lock().unwrap();
    credits.get_mut(repo).expect("a credit was granted").until =
        Instant::now() + Duration::from_secs(86_400);
}

/// Every child the gate actually started, so a test can count spawns in its
/// own repository instead of inferring them from timing.
///
/// Kept per working directory. The suite shares one process, and a log shared
/// by every test is evicted by whichever tests spawn the most: a test counting
/// its own children by position (`spawns_in(cwd).len()` before and after) then
/// lost its oldest entries to its neighbours and counted short, more often the
/// longer it ran, which is to say under load. A test's directory is only ever
/// pushed to by that test, so its entries are evicted by nothing but its own
/// volume, and a directory that has gone quiet is the first to be dropped.
#[cfg(test)]
pub(crate) mod spawn_log {
    use std::collections::{HashMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::Mutex;

    use super::Admission;

    /// Entries kept for one directory, and directories kept at once.
    const PER_DIRECTORY: usize = 16_384;
    const DIRECTORIES: usize = 1_024;
    type Entry = (Vec<String>, Admission);

    struct Log {
        per_directory: usize,
        directories: usize,
        by_directory: HashMap<PathBuf, (u64, VecDeque<Entry>)>,
        touched: u64,
    }

    impl Log {
        fn bounded(per_directory: usize, directories: usize) -> Self {
            Self {
                per_directory,
                directories,
                by_directory: HashMap::new(),
                touched: 0,
            }
        }

        fn push(&mut self, cwd: &Path, entry: Entry) {
            let cwd = directory_key(cwd);
            let cwd = cwd.as_path();
            self.touched += 1;
            let touched = self.touched;
            if !self.by_directory.contains_key(cwd) && self.by_directory.len() >= self.directories {
                // The directory nothing has spawned in for longest.
                if let Some(stale) = self
                    .by_directory
                    .iter()
                    .min_by_key(|(_, (last, _))| *last)
                    .map(|(dir, _)| dir.clone())
                {
                    self.by_directory.remove(&stale);
                }
            }
            let (last, entries) = self
                .by_directory
                .entry(cwd.to_path_buf())
                .or_insert_with(|| (touched, VecDeque::new()));
            *last = touched;
            if entries.len() == self.per_directory {
                entries.pop_front();
            }
            entries.push_back(entry);
        }

        fn entries(&self, cwd: &Path) -> Vec<Entry> {
            self.by_directory
                .get(directory_key(cwd).as_path())
                .map(|(_, entries)| entries.iter().cloned().collect())
                .unwrap_or_default()
        }
    }

    /// A directory keyed by one spelling. The repository identity spells a
    /// checkout `\\?\C:\...` on Windows while git lists the same directory
    /// `C:/...`, and a test counting a repository's children must find them
    /// whichever of the two spawned them or asks.
    fn directory_key(cwd: &Path) -> PathBuf {
        match cwd.to_str() {
            Some(text) => PathBuf::from(crate::engine::process::plain_path_text(text)),
            None => cwd.to_path_buf(),
        }
    }

    static LOG: Mutex<Option<Log>> = Mutex::new(None);

    pub(in crate::engine::process) fn record(cmd: &Command) {
        let Some(cwd) = cmd.get_current_dir() else {
            return;
        };
        let argv = cmd
            .get_args()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect();
        let class = super::current_admission();
        LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get_or_insert_with(|| Log::bounded(PER_DIRECTORY, DIRECTORIES))
            .push(cwd, (argv, class));
    }

    /// Argument vectors of every recorded child whose working directory was
    /// `cwd`, oldest first.
    pub(crate) fn spawns_in(cwd: &Path) -> Vec<Vec<String>> {
        classed_spawns_in(cwd)
            .into_iter()
            .map(|(argv, _)| argv)
            .collect()
    }

    /// [`spawns_in`], with the admission class of the thread that spawned
    /// each child.
    pub(crate) fn classed_spawns_in(cwd: &Path) -> Vec<(Vec<String>, Admission)> {
        LOG.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .as_ref()
            .map(|log| log.entries(cwd))
            .unwrap_or_default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn entry() -> Entry {
            (vec!["status".into()], Admission::Reactive)
        }

        /// The defect: a count taken by position was shortened by other tests'
        /// spawns. One directory's volume no longer evicts another's entries.
        #[test]
        fn a_busy_neighbour_does_not_evict_a_quiet_directory() {
            let mut log = Log::bounded(8, 4);
            let (quiet, busy) = (Path::new("/quiet"), Path::new("/busy"));
            for _ in 0..3 {
                log.push(quiet, entry());
            }
            for _ in 0..100 {
                log.push(busy, entry());
            }
            assert_eq!(log.entries(quiet).len(), 3);
            assert_eq!(log.entries(busy).len(), 8, "bounded by its own volume");
        }

        /// A child spawned in the identity spelling of a directory is counted
        /// by a test that names it the way git lists it, and the other way
        /// round: on Windows `\\?\C:\x` and `C:\x` are one directory.
        #[test]
        fn one_directory_is_one_key_whichever_spelling_names_it() {
            let mut log = Log::bounded(8, 4);
            let (verbatim, plain) = if cfg!(windows) {
                (Path::new(r"\\?\C:\gp\repo"), Path::new(r"C:\gp\repo"))
            } else {
                (Path::new("/gp/repo"), Path::new("/gp/repo"))
            };
            log.push(verbatim, entry());
            log.push(plain, entry());
            assert_eq!(log.entries(verbatim).len(), 2);
            assert_eq!(log.entries(plain).len(), 2);
        }

        #[test]
        fn the_directory_that_went_quiet_is_the_one_dropped() {
            let mut log = Log::bounded(8, 3);
            let (first, live) = (Path::new("/first"), Path::new("/live"));
            log.push(first, entry());
            log.push(live, entry());
            for other in ["/a", "/b", "/c", "/d"] {
                log.push(Path::new(other), entry());
                // A live directory keeps spawning, so it stays.
                log.push(live, entry());
            }
            assert!(log.entries(first).is_empty(), "the quiet one went");
            assert!(!log.entries(live).is_empty(), "the busy one stayed");
            assert_eq!(log.by_directory.len(), 3, "never more than its bound");
        }
    }
}
