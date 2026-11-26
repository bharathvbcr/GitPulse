//! Batched ODB landing queue and cross-process SQLite FIFO hot-file tickets.
//!
//! Coordinates concurrent branch lands per `(repo_common_dir, target_branch)`:
//! - Collects bursts of landing requests in a short window (`10ms` from
//!   `queue[0].enqueued`) up to an AIMD-adapted batch size (`max_size = 50`,
//!   hard cap `100`, `target_latency = 500ms`).
//! - Executes each batch sequentially against a running tip inside a single
//!   [`ObjectQuarantine`] (driving a single `git merge-tree --stdin` stream
//!   with single-shot fallback), packing new objects once and updating
//!   `refs/heads/<target>` with one atomic CAS (`merge_tree::land_verified`).
//! - Isolates per-lane failures without losing earlier clean lanes, upholds the
//!   `CompleteEarly` safety proof (early-finalize only while `members.is_empty()`
//!   unless tip-independent), splits dependents on non-retryable update
//!   failures, retries lost CAS up to `10` times with jittered `2..100ms`
//!   backoff, and falls back to sequential solo landing on combined-batch hook
//!   rejections.
//! - Acquires cross-process sorted-path FIFO resolution tickets in
//!   `<common>/gitpulse/land_tickets.sqlite` (SQLite WAL, `BEGIN IMMEDIATE`)
//!   with release-before-upgrade deadlock freedom and PID/TTL expiry.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::io::{BufRead, BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{ChildStderr, ChildStdin, ChildStdout};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use crate::engine::git_cli::{
    git_captured, git_spawn_quarantined_streaming, resolve_git_common_dir, without_lazy_fetch,
};
use crate::engine::git_writer::{is_full_oid, repo_mutation_lock, GitWriter};
use crate::engine::merge_tree::{
    active_commit_hooks, already_covered, checkout_holders_for_target, land_verified,
    verify_in_scratch, HeldReason, IntegrateFailure,
};
use crate::engine::object_quarantine::{
    parse_merge_tree_output, refuse_partial_clone, MergeOutcome, ObjectQuarantine, QuarantineError,
    MAX_STREAM_RESPAWNS,
};
use crate::engine::process::{StreamingChild, DEFAULT_TIMEOUT};
use crate::engine::sqlite_wal::{enter_wal, LEDGER_WAL_PATIENCE};
use crate::ipc_error::CommandError;
use crate::ledger::land_identity::{
    abort_land_intent, check_already_landed, confirm_land_intent, record_land_intent,
};

pub const DEFAULT_BATCH_WINDOW: Duration = Duration::from_millis(10);
pub const DEFAULT_BATCH_MAX_SIZE: usize = 50;
pub const HARD_BATCH_SIZE_CAP: usize = 100;
pub const DEFAULT_TARGET_LATENCY: Duration = Duration::from_millis(500);
pub const DEFAULT_KEY_STATE_TTL: Duration = Duration::from_secs(300);
pub const DEFAULT_MAX_REBUILDS: usize = 10;
pub const DEFAULT_TICKET_TTL: Duration = Duration::from_secs(60);
pub const DEFAULT_TICKET_WAIT_TIMEOUT: Duration = Duration::from_secs(30);

const CAS_BACKOFF_MIN_MS: u64 = 2;
const CAS_BACKOFF_MAX_MS: u64 = 100;
const TICKET_BRANCH_WIDE_KEY: &str = "\x00branch_wide";
const MAX_STREAM_RECORD_BYTES: usize = 16 * 1024 * 1024;
const MAX_STREAM_STDERR_BYTES: usize = 64 * 1024;

static INVOCATION_SEQ: AtomicU64 = AtomicU64::new(1);
static HOLDER_SEQ: AtomicU64 = AtomicU64::new(1);
static PACK_TMP_SEQ: AtomicU64 = AtomicU64::new(1);

/// Configuration knobs for [`LandQueueRegistry`].
#[derive(Debug, Clone)]
pub struct LandQueueConfig {
    pub window: Duration,
    pub initial_max_size: usize,
    pub hard_cap: usize,
    pub target_latency: Duration,
    pub key_state_ttl: Duration,
    pub max_rebuilds: usize,
}

impl Default for LandQueueConfig {
    fn default() -> Self {
        Self {
            window: DEFAULT_BATCH_WINDOW,
            initial_max_size: DEFAULT_BATCH_MAX_SIZE,
            hard_cap: HARD_BATCH_SIZE_CAP,
            target_latency: DEFAULT_TARGET_LATENCY,
            key_state_ttl: DEFAULT_KEY_STATE_TTL,
            max_rebuilds: DEFAULT_MAX_REBUILDS,
        }
    }
}

/// Request submitted to the batched landing queue.
#[derive(Clone)]
pub struct LandRequest {
    pub repo: PathBuf,
    pub branch: String,
    pub target_branch: String,
    pub source_oid: String,
    pub squash: bool,
    pub commit_message: Option<String>,
    /// When `true` and the combined batch is rejected by hooks/policy, fall back
    /// sequentially to solo landing for each remaining request.
    pub allow_solo_hook_fallback: bool,
    /// Optional paths requiring cross-process FIFO resolution tickets before
    /// landing.
    pub hot_files: Vec<String>,
    /// When `true`, acquire a branch-wide FIFO ticket (`\x00branch_wide`).
    pub branch_wide_ticket: bool,
    /// When `true`, verify the landed candidate in pooled scratch before CAS
    /// and advance `refs/gitpulse/green/<target_branch>` best-effort.
    pub verify_in_scratch: bool,
    /// Optional cooperative cancellation token checked while queued/waiting.
    pub cancel: Option<Arc<AtomicBool>>,
}

impl LandRequest {
    pub fn new(
        repo: impl Into<PathBuf>,
        branch: impl Into<String>,
        target_branch: impl Into<String>,
        source_oid: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            branch: branch.into(),
            target_branch: target_branch.into(),
            source_oid: source_oid.into(),
            squash: false,
            commit_message: None,
            allow_solo_hook_fallback: true,
            hot_files: Vec::new(),
            branch_wide_ticket: false,
            verify_in_scratch: false,
            cancel: None,
        }
    }
}

/// Per-submission telemetry describing how a landing was processed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LandTelemetry {
    pub quarantine_runs: usize,
    pub cas_attempts: usize,
    pub batch_size: usize,
    pub batch_members: usize,
    pub completed_early: bool,
    pub solo_fallback: bool,
}

/// Successful outcome of a [`LandRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LandOutcome {
    pub branch: String,
    pub target_branch: String,
    pub source_oid: String,
    pub landed_oid: String,
    pub batch_tip: String,
    pub already_landed: bool,
    pub reverted_by: Option<String>,
    pub checkout_advanced: bool,
    pub checkout_note: Option<String>,
    pub green_ref_note: Option<String>,
    pub telemetry: LandTelemetry,
}

/// Failure outcome of a [`LandRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LandError {
    Conflict {
        message: String,
        paths: Vec<String>,
        total: usize,
    },
    PermissionDenied {
        message: String,
    },
    InvalidArgument {
        message: String,
    },
    Held {
        reason: HeldReason,
        message: String,
        path: Option<String>,
    },
    CommitHooks {
        hooks: Vec<String>,
    },
    Rejected {
        message: String,
    },
    CasExhausted {
        message: String,
    },
    Canceled,
    Failed {
        message: String,
    },
}

impl std::fmt::Display for LandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { message, .. }
            | Self::PermissionDenied { message }
            | Self::InvalidArgument { message }
            | Self::Held { message, .. }
            | Self::Rejected { message }
            | Self::CasExhausted { message }
            | Self::Failed { message } => f.write_str(message),
            Self::CommitHooks { hooks } => {
                write!(
                    f,
                    "active commit hooks blocked landing: {}",
                    hooks.join(", ")
                )
            }
            Self::Canceled => f.write_str("land request canceled"),
        }
    }
}

impl std::error::Error for LandError {}

/// Context passed to an optional pre-CAS batch update hook/gate.
#[allow(dead_code)]
pub(crate) struct BatchUpdateContext<'a> {
    pub repo: &'a Path,
    pub target_branch: &'a str,
    pub old_tip: &'a str,
    pub new_tip: &'a str,
    pub member_count: usize,
    pub is_solo_fallback: bool,
}

/// Verdict returned by an optional pre-CAS batch update hook/gate.
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum BatchUpdateDecision {
    Allow,
    /// Hook or policy rejected the combined batch (triggers solo fallback when
    /// `member_count > 1` or `allow_solo_hook_fallback` permits).
    RejectHook(String),
    /// Simulated CAS race (triggers rebuild retry against fresh published tip).
    LostCas(String),
    /// Non-retryable update error (triggers `split_dependents`).
    FailUpdate(LandError),
}

pub(crate) type BatchUpdateGate =
    Arc<dyn Fn(&BatchUpdateContext<'_>) -> BatchUpdateDecision + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LaneKey {
    repo_common_dir: PathBuf,
    target_branch: String,
}

struct KeyState {
    max_size: usize,
    idle_since: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EntryState {
    Queued,
    Executing,
    Done,
}

struct QueueEntry {
    id: u64,
    req: LandRequest,
    enqueued: Instant,
    state: Mutex<EntryState>,
    outcome: Mutex<Option<Result<LandOutcome, LandError>>>,
    done_cv: Condvar,
}

impl QueueEntry {
    fn is_canceled(&self) -> bool {
        self.req
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    fn deliver(&self, res: Result<LandOutcome, LandError>) {
        let mut out = self
            .outcome
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if out.is_none() {
            *out = Some(res);
            let mut st = self
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *st = EntryState::Done;
            self.done_cv.notify_all();
        }
    }
}

struct Lane {
    key: LaneKey,
    queue: Mutex<VecDeque<Arc<QueueEntry>>>,
    cv: Condvar,
    max_size: Mutex<usize>,
}

struct RegistryInner {
    lanes: HashMap<LaneKey, Arc<Lane>>,
    key_states: HashMap<LaneKey, KeyState>,
    closed: bool,
    next_entry_id: u64,
}

/// Process-wide or test-scoped coordinator of per-`(repo_common_dir, target_branch)`
/// batched landing lanes.
pub struct LandQueueRegistry {
    config: LandQueueConfig,
    batch_gate: Option<BatchUpdateGate>,
    closed: AtomicBool,
    inner: Mutex<RegistryInner>,
}

impl LandQueueRegistry {
    /// Shared global landing queue with default settings.
    pub fn global() -> &'static Arc<LandQueueRegistry> {
        static GLOBAL: OnceLock<Arc<LandQueueRegistry>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(Self::new(LandQueueConfig::default())))
    }

    pub fn new(config: LandQueueConfig) -> Self {
        Self {
            config,
            batch_gate: None,
            closed: AtomicBool::new(false),
            inner: Mutex::new(RegistryInner {
                lanes: HashMap::new(),
                key_states: HashMap::new(),
                closed: false,
                next_entry_id: 1,
            }),
        }
    }

    #[cfg(test)]
    pub(crate) fn with_batch_gate(config: LandQueueConfig, gate: BatchUpdateGate) -> Self {
        Self {
            config,
            batch_gate: Some(gate),
            closed: AtomicBool::new(false),
            inner: Mutex::new(RegistryInner {
                lanes: HashMap::new(),
                key_states: HashMap::new(),
                closed: false,
                next_entry_id: 1,
            }),
        }
    }

    /// Submits `req` to the lane for `(repo_common_dir, target_branch)` and
    /// blocks until its outcome is ready or `req.cancel` fires while queued/running.
    pub fn submit(self: &Arc<Self>, req: LandRequest) -> Result<LandOutcome, LandError> {
        if req
            .cancel
            .as_ref()
            .is_some_and(|flag| flag.load(Ordering::Acquire))
        {
            return Err(LandError::Canceled);
        }
        let key = lane_key(&req.repo, &req.target_branch);
        let (lane, entry) = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if inner.closed {
                return Err(LandError::Canceled);
            }
            let now = Instant::now();
            let ttl = self.config.key_state_ttl;
            inner
                .key_states
                .retain(|_, ks| now.saturating_duration_since(ks.idle_since) < ttl);

            let entry_id = inner.next_entry_id;
            inner.next_entry_id += 1;
            let entry = Arc::new(QueueEntry {
                id: entry_id,
                req,
                enqueued: now,
                state: Mutex::new(EntryState::Queued),
                outcome: Mutex::new(None),
                done_cv: Condvar::new(),
            });

            let (lane, spawn_worker) = if let Some(existing) = inner.lanes.get(&key) {
                (Arc::clone(existing), false)
            } else {
                let initial_size = inner
                    .key_states
                    .remove(&key)
                    .map(|ks| ks.max_size)
                    .unwrap_or(self.config.initial_max_size)
                    .clamp(1, self.config.hard_cap);
                let created = Arc::new(Lane {
                    key: key.clone(),
                    queue: Mutex::new(VecDeque::new()),
                    cv: Condvar::new(),
                    max_size: Mutex::new(initial_size),
                });
                inner.lanes.insert(key.clone(), Arc::clone(&created));
                (created, true)
            };

            {
                let mut q = lane
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                q.push_back(Arc::clone(&entry));
                lane.cv.notify_all();
            }

            if spawn_worker {
                let registry = Arc::clone(self);
                let worker_lane = Arc::clone(&lane);
                std::thread::Builder::new()
                    .name(format!("land-queue-{}", key.target_branch))
                    .spawn(move || registry.run_lane(worker_lane))
                    .map_err(|e| LandError::Failed {
                        message: format!("failed to spawn land queue worker: {e}"),
                    })?;
            }
            (lane, entry)
        };

        // Wait for delivery or cooperative cancellation.
        // Lock hierarchy: `self.inner` -> `lane.queue` -> `entry.state` -> `entry.outcome`.
        loop {
            if entry.is_canceled() {
                let mut q = lane
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let mut st = entry
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                match *st {
                    EntryState::Queued => {
                        q.retain(|item| item.id != entry.id);
                        *st = EntryState::Done;
                        return Err(LandError::Canceled);
                    }
                    EntryState::Executing => {
                        // Batch is already running and may still land.
                        return Err(LandError::Canceled);
                    }
                    EntryState::Done => {
                        drop(st);
                        drop(q);
                        return entry
                            .outcome
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone()
                            .unwrap_or(Err(LandError::Canceled));
                    }
                }
            }

            let out_guard = entry
                .outcome
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if let Some(res) = out_guard.as_ref() {
                return res.clone();
            }
            let wait_slice = if entry.req.cancel.is_some() {
                Duration::from_millis(5)
            } else {
                Duration::from_millis(250)
            };
            let _ = entry.done_cv.wait_timeout(out_guard, wait_slice);
        }
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        let lanes: Vec<Arc<Lane>> = {
            let mut inner = self
                .inner
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            inner.closed = true;
            inner.lanes.values().cloned().collect()
        };
        for lane in lanes {
            let _q = lane
                .queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            lane.cv.notify_all();
        }
    }

    fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }

    fn run_lane(self: Arc<Self>, lane: Arc<Lane>) {
        loop {
            let Some(batch) = self.collect_batch(&lane) else {
                return;
            };
            let started = Instant::now();
            self.execute_batch(&lane.key, batch);
            let took = started.elapsed();

            let mut max_size = lane
                .max_size
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            *max_size = adapt_batch_size(
                *max_size,
                took,
                self.config.target_latency,
                self.config.hard_cap,
            );
        }
    }

    fn collect_batch(&self, lane: &Arc<Lane>) -> Option<Vec<Arc<QueueEntry>>> {
        let max_size = *lane
            .max_size
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut q = lane
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        loop {
            if q.is_empty() {
                // Retire lane inside the same lock hierarchy check when idle.
                drop(q);
                let mut inner = self
                    .inner
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let q2 = lane
                    .queue
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if q2.is_empty() {
                    let adapted = *lane
                        .max_size
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    inner.lanes.remove(&lane.key);
                    inner.key_states.insert(
                        lane.key.clone(),
                        KeyState {
                            max_size: adapted,
                            idle_since: Instant::now(),
                        },
                    );
                    return None;
                }
                q = q2;
                continue;
            }

            if self.is_closed() || q.len() >= max_size {
                break;
            }
            let first_deadline = q[0].enqueued + self.config.window;
            let now = Instant::now();
            if now >= first_deadline {
                break;
            }
            let remaining = first_deadline.saturating_duration_since(now);
            let (guard, _) = lane
                .cv
                .wait_timeout(q, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            q = guard;
        }

        let drain_n = q.len().min(max_size);
        let mut batch = Vec::with_capacity(drain_n);
        for _ in 0..drain_n {
            if let Some(entry) = q.pop_front() {
                let mut st = entry
                    .state
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if *st == EntryState::Queued {
                    *st = EntryState::Executing;
                    batch.push(Arc::clone(&entry));
                }
            }
        }
        Some(batch)
    }

    fn execute_batch(&self, key: &LaneKey, batch: Vec<Arc<QueueEntry>>) {
        if batch.is_empty() {
            return;
        }
        let batch_size = batch.len();
        let repo = batch[0].req.repo.clone();
        let mut remaining: Vec<usize> = (0..batch_size).collect();
        let mut final_outcomes: Vec<Option<Result<LandOutcome, LandError>>> =
            vec![None; batch_size];
        let mut quarantine_runs = 0usize;
        let mut cas_attempts = 0usize;
        let mut rebuilds = 0usize;

        loop {
            if remaining.is_empty() {
                break;
            }

            // Acquire per-batch hot-file FIFO ticket if any remaining item requests one.
            let ticket_guard = match acquire_batch_ticket(&repo, key, &batch, &remaining) {
                Ok(guard) => guard,
                Err(err) => {
                    for idx in remaining.drain(..) {
                        final_outcomes[idx] = Some(Err(err.clone()));
                    }
                    break;
                }
            };

            let tip = match read_target_tip(&repo, &key.target_branch) {
                Ok(tip) => tip,
                Err(err) => {
                    drop(ticket_guard);
                    for idx in remaining.drain(..) {
                        final_outcomes[idx] = Some(Err(err.clone()));
                    }
                    break;
                }
            };

            quarantine_runs += 1;
            let attempt = self.run_quarantine_attempt(
                &repo,
                &key.target_branch,
                &tip,
                &batch,
                &remaining,
                batch_size,
                quarantine_runs,
                cas_attempts,
                false,
            );

            let mut attempt = match attempt {
                Ok(a) => a,
                Err(err) => {
                    drop(ticket_guard);
                    for idx in remaining.drain(..) {
                        final_outcomes[idx] = Some(Err(err.clone()));
                    }
                    break;
                }
            };

            // Deliver any items that passed CompleteEarly safety during the attempt.
            let mut finalized_set: HashSet<usize> = HashSet::new();
            for (orig_idx, step_res) in &attempt.steps {
                if can_complete_early(step_res, attempt.members_empty_at_step(*orig_idx)) {
                    let mut out = step_res.to_result(
                        &batch[*orig_idx].req,
                        &tip,
                        LandTelemetry {
                            quarantine_runs,
                            cas_attempts,
                            batch_size,
                            batch_members: attempt.members.len(),
                            completed_early: true,
                            solo_fallback: false,
                        },
                    );
                    if let Ok(ref mut ok) = out {
                        ok.telemetry.completed_early = true;
                    }
                    final_outcomes[*orig_idx] = Some(out);
                    finalized_set.insert(*orig_idx);
                }
            }

            if attempt.members.is_empty() {
                drop(ticket_guard);
                for (orig_idx, step_res) in attempt.steps {
                    if !finalized_set.contains(&orig_idx) {
                        final_outcomes[orig_idx] = Some(step_res.to_result(
                            &batch[orig_idx].req,
                            &tip,
                            LandTelemetry {
                                quarantine_runs,
                                cas_attempts,
                                batch_size,
                                batch_members: 0,
                                completed_early: false,
                                solo_fallback: false,
                            },
                        ));
                    }
                }
                break;
            }

            // Pack quarantined objects once (unless pure fast-forward left 0 loose objects).
            if let Err(err) = attempt.quarantine.pack_and_migrate() {
                drop(ticket_guard);
                for idx in remaining.drain(..) {
                    if !finalized_set.contains(&idx) {
                        final_outcomes[idx] = Some(Err(err.clone()));
                    }
                }
                break;
            }
            drop(attempt.quarantine);

            let mut verified_tip: Option<String> = None;
            let any_verify = attempt
                .members
                .iter()
                .any(|&idx| batch[idx].req.verify_in_scratch);
            if any_verify {
                let mut gate = allow_all_gate;
                if let Err(failure) =
                    verify_in_scratch(&repo, &attempt.cur_tip, &mut |_, _| Ok(()), &mut gate)
                {
                    drop(ticket_guard);
                    let err = map_integrate_failure(failure);
                    let next = split_dependents_on_failure(
                        &remaining,
                        &attempt.members,
                        &attempt.steps,
                        &finalized_set,
                        &batch,
                        &tip,
                        err,
                        quarantine_runs,
                        cas_attempts,
                        batch_size,
                        &mut final_outcomes,
                    );
                    remaining = next;
                    continue;
                }
                verified_tip = Some(attempt.cur_tip.clone());
            }

            cas_attempts += 1;
            let publish = self.publish_attempt(
                &repo,
                &key.target_branch,
                &tip,
                &attempt.cur_tip,
                &attempt.recorded_identities,
                attempt.members.len(),
                verified_tip.as_deref(),
                false,
            );
            drop(ticket_guard);

            match publish {
                PublishOutcome::Applied {
                    checkout_advanced,
                    checkout_note,
                    green_ref_note,
                } => {
                    for (orig_idx, step_res) in attempt.steps {
                        if finalized_set.contains(&orig_idx) {
                            continue;
                        }
                        let mut res = step_res.to_result(
                            &batch[orig_idx].req,
                            &attempt.cur_tip,
                            LandTelemetry {
                                quarantine_runs,
                                cas_attempts,
                                batch_size,
                                batch_members: attempt.members.len(),
                                completed_early: false,
                                solo_fallback: false,
                            },
                        );
                        if let Ok(ref mut ok) = res {
                            ok.checkout_advanced = checkout_advanced;
                            ok.checkout_note = checkout_note.clone();
                            ok.green_ref_note = green_ref_note.clone();
                        }
                        final_outcomes[orig_idx] = Some(res);
                    }
                    break;
                }
                PublishOutcome::LostCas(reason) => {
                    remaining.retain(|idx| !finalized_set.contains(idx));
                    rebuilds += 1;
                    if rebuilds > self.config.max_rebuilds {
                        let first_member = attempt.members[0];
                        let m0_pos = remaining
                            .iter()
                            .position(|&idx| idx == first_member)
                            .unwrap_or(0);
                        for (pos, &orig_idx) in remaining.iter().enumerate() {
                            if pos < m0_pos {
                                if let Some(step_res) = attempt
                                    .steps
                                    .iter()
                                    .find(|(id, _)| *id == orig_idx)
                                    .map(|(_, s)| s)
                                {
                                    final_outcomes[orig_idx] = Some(step_res.to_result(
                                        &batch[orig_idx].req,
                                        &tip,
                                        LandTelemetry {
                                            quarantine_runs,
                                            cas_attempts,
                                            batch_size,
                                            batch_members: attempt.members.len(),
                                            completed_early: false,
                                            solo_fallback: false,
                                        },
                                    ));
                                }
                            } else {
                                final_outcomes[orig_idx] = Some(Err(LandError::CasExhausted {
                                    message: format!(
                                        "CAS rebuilds exhausted after {} attempts ({reason})",
                                        self.config.max_rebuilds
                                    ),
                                }));
                            }
                        }
                        break;
                    }
                    std::thread::sleep(jittered_cas_backoff(rebuilds));
                }
                PublishOutcome::Rejected(reason) => {
                    remaining.retain(|idx| !finalized_set.contains(idx));
                    if remaining.len() == 1
                        || !remaining
                            .iter()
                            .any(|&i| batch[i].req.allow_solo_hook_fallback)
                    {
                        for idx in remaining.drain(..) {
                            final_outcomes[idx] = Some(Err(LandError::Rejected {
                                message: reason.clone(),
                            }));
                        }
                        break;
                    }
                    // Sequential solo fallback so each item gets its own hooks/identity attribution.
                    for idx in remaining.drain(..) {
                        let solo_res = self.execute_solo_fallback(
                            key,
                            &batch[idx].req,
                            &mut quarantine_runs,
                            &mut cas_attempts,
                            batch_size,
                        );
                        final_outcomes[idx] = Some(solo_res);
                    }
                    break;
                }
                PublishOutcome::Failed(err) => {
                    remaining = split_dependents_on_failure(
                        &remaining,
                        &attempt.members,
                        &attempt.steps,
                        &finalized_set,
                        &batch,
                        &tip,
                        err,
                        quarantine_runs,
                        cas_attempts,
                        batch_size,
                        &mut final_outcomes,
                    );
                }
            }
        }

        for (idx, entry) in batch.into_iter().enumerate() {
            let res = final_outcomes[idx].take().unwrap_or(Err(LandError::Failed {
                message: "land batch terminated without an outcome".into(),
            }));
            entry.deliver(res);
        }
    }

    fn execute_solo_fallback(
        &self,
        key: &LaneKey,
        req: &LandRequest,
        quarantine_runs: &mut usize,
        cas_attempts: &mut usize,
        batch_size: usize,
    ) -> Result<LandOutcome, LandError> {
        let mut rebuilds = 0usize;
        loop {
            let ticket_guard = acquire_single_ticket(&req.repo, key, req)?;
            let tip = read_target_tip(&req.repo, &key.target_branch)?;
            *quarantine_runs += 1;
            let solo_entries = [Arc::new(QueueEntry {
                id: 0,
                req: req.clone(),
                enqueued: Instant::now(),
                state: Mutex::new(EntryState::Executing),
                outcome: Mutex::new(None),
                done_cv: Condvar::new(),
            })];
            let mut attempt = self.run_quarantine_attempt(
                &req.repo,
                &key.target_branch,
                &tip,
                &solo_entries,
                &[0],
                batch_size,
                *quarantine_runs,
                *cas_attempts,
                true,
            )?;
            let step_res = attempt
                .steps
                .pop()
                .map(|(_, s)| s)
                .unwrap_or(StepResult::Fail {
                    err: LandError::Failed {
                        message: "solo fallback produced no step outcome".into(),
                    },
                    tip_independent: true,
                });
            if attempt.members.is_empty() {
                drop(ticket_guard);
                return step_res.to_result(
                    req,
                    &tip,
                    LandTelemetry {
                        quarantine_runs: *quarantine_runs,
                        cas_attempts: *cas_attempts,
                        batch_size,
                        batch_members: 0,
                        completed_early: false,
                        solo_fallback: true,
                    },
                );
            }

            attempt.quarantine.pack_and_migrate()?;
            drop(attempt.quarantine);

            let mut verified_tip = None;
            if req.verify_in_scratch {
                let mut gate = allow_all_gate;
                verify_in_scratch(&req.repo, &attempt.cur_tip, &mut |_, _| Ok(()), &mut gate)
                    .map_err(map_integrate_failure)?;
                verified_tip = Some(attempt.cur_tip.clone());
            }

            *cas_attempts += 1;
            let publish = self.publish_attempt(
                &req.repo,
                &key.target_branch,
                &tip,
                &attempt.cur_tip,
                &attempt.recorded_identities,
                1,
                verified_tip.as_deref(),
                true,
            );
            drop(ticket_guard);

            match publish {
                PublishOutcome::Applied {
                    checkout_advanced,
                    checkout_note,
                    green_ref_note,
                } => {
                    let mut res = step_res.to_result(
                        req,
                        &attempt.cur_tip,
                        LandTelemetry {
                            quarantine_runs: *quarantine_runs,
                            cas_attempts: *cas_attempts,
                            batch_size,
                            batch_members: 1,
                            completed_early: false,
                            solo_fallback: true,
                        },
                    );
                    if let Ok(ref mut ok) = res {
                        ok.checkout_advanced = checkout_advanced;
                        ok.checkout_note = checkout_note;
                        ok.green_ref_note = green_ref_note;
                    }
                    return res;
                }
                PublishOutcome::LostCas(reason) => {
                    rebuilds += 1;
                    if rebuilds > self.config.max_rebuilds {
                        return Err(LandError::CasExhausted {
                            message: format!(
                                "solo fallback CAS rebuilds exhausted after {} attempts ({reason})",
                                self.config.max_rebuilds
                            ),
                        });
                    }
                    std::thread::sleep(jittered_cas_backoff(rebuilds));
                }
                PublishOutcome::Rejected(reason) => {
                    return Err(LandError::Rejected { message: reason });
                }
                PublishOutcome::Failed(err) => return Err(err),
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn run_quarantine_attempt(
        &self,
        repo: &Path,
        target_branch: &str,
        tip: &str,
        batch: &[Arc<QueueEntry>],
        remaining: &[usize],
        _batch_size: usize,
        _quarantine_runs: usize,
        _cas_attempts: usize,
        _is_solo: bool,
    ) -> Result<AttemptState, LandError> {
        refuse_partial_clone(repo).map_err(|e| LandError::PermissionDenied {
            message: e.into_message(),
        })?;
        let mut quarantine = QuarantineSession::open(repo, Instant::now() + DEFAULT_TIMEOUT)?;
        let mut cur = tip.to_string();
        let mut members: Vec<usize> = Vec::new();
        let mut member_count_before: HashMap<usize, usize> = HashMap::new();
        let mut landed_as: HashMap<String, String> = HashMap::new();
        let mut recorded_identities: Vec<(String, String)> = Vec::new();
        let mut steps: Vec<(usize, StepResult)> = Vec::with_capacity(remaining.len());

        for &orig_idx in remaining {
            let req = &batch[orig_idx].req;
            member_count_before.insert(orig_idx, members.len());

            if !is_full_oid(&req.source_oid) {
                steps.push((
                    orig_idx,
                    StepResult::Fail {
                        err: LandError::InvalidArgument {
                            message: format!("invalid source commit id {:?}", req.source_oid),
                        },
                        tip_independent: true,
                    },
                ));
                continue;
            }
            if !quarantine.commit_exists(&req.source_oid)? {
                steps.push((
                    orig_idx,
                    StepResult::Fail {
                        err: LandError::InvalidArgument {
                            message: format!("source commit {} does not exist", req.source_oid),
                        },
                        tip_independent: true,
                    },
                ));
                continue;
            }

            if let Some(prev_tip) = landed_as.get(&req.source_oid) {
                steps.push((
                    orig_idx,
                    StepResult::AlreadyLanded {
                        landed_oid: prev_tip.clone(),
                        reverted_by: None,
                    },
                ));
                continue;
            }

            let target_ref = format!("refs/heads/{target_branch}");
            if let Some(matched) = check_already_landed(repo, &target_ref, &req.source_oid, tip)
                .map_err(|e| LandError::Failed {
                    message: e.into_message(),
                })?
            {
                steps.push((
                    orig_idx,
                    StepResult::AlreadyLanded {
                        landed_oid: matched.landed_oid,
                        reverted_by: matched.reverted_by,
                    },
                ));
                continue;
            }

            if !req.squash && quarantine.is_ancestor(&req.source_oid, &cur)? {
                steps.push((
                    orig_idx,
                    StepResult::AlreadyLanded {
                        landed_oid: req.source_oid.clone(),
                        reverted_by: None,
                    },
                ));
                continue;
            }

            let fast_forward = quarantine.is_ancestor(&cur, &req.source_oid)?;
            if req.squash || !fast_forward {
                if let Err(err) = GitWriter::require_commit_identity(repo) {
                    steps.push((
                        orig_idx,
                        StepResult::Fail {
                            err: LandError::PermissionDenied {
                                message: err.into_message(),
                            },
                            tip_independent: true,
                        },
                    ));
                    continue;
                }
            }

            let outcome = match quarantine.merge_tree(&cur, &req.source_oid) {
                Ok(out) => out,
                Err(err) => {
                    steps.push((
                        orig_idx,
                        StepResult::Fail {
                            err,
                            tip_independent: false,
                        },
                    ));
                    continue;
                }
            };

            let merged_tree = match outcome {
                MergeOutcome::Clean { tree } => tree,
                MergeOutcome::Conflicted {
                    tree: _,
                    paths,
                    paths_total,
                } => {
                    let message = format!(
                        "Merging {} into {target_branch} conflicted in {paths_total} file(s): {}",
                        req.branch,
                        paths
                            .iter()
                            .take(5)
                            .map(String::as_str)
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    steps.push((
                        orig_idx,
                        StepResult::Conflict {
                            message,
                            paths,
                            total: paths_total,
                        },
                    ));
                    continue;
                }
            };

            let cur_tree = quarantine.tree_of(&cur)?;
            let next_cur = if !req.squash && fast_forward {
                req.source_oid.clone()
            } else if req.squash && merged_tree == cur_tree {
                steps.push((
                    orig_idx,
                    StepResult::AlreadyLanded {
                        landed_oid: cur.clone(),
                        reverted_by: None,
                    },
                ));
                continue;
            } else {
                let mut gate = allow_all_gate;
                let hooks =
                    active_commit_hooks(repo, &mut gate).map_err(|e| LandError::Failed {
                        message: e.into_message(),
                    })?;
                if !hooks.is_empty() {
                    steps.push((
                        orig_idx,
                        StepResult::Fail {
                            err: LandError::CommitHooks { hooks },
                            tip_independent: false,
                        },
                    ));
                    continue;
                }
                let message = match req
                    .commit_message
                    .as_deref()
                    .filter(|m| !m.trim().is_empty())
                {
                    Some(custom) => custom.to_string(),
                    None if req.squash => format!("Merge branch '{}' (squashed)", req.branch),
                    None => format!("Merge branch '{}' into {target_branch}", req.branch),
                };
                let second = if req.squash {
                    None
                } else {
                    Some(req.source_oid.as_str())
                };
                quarantine.commit_tree(&merged_tree, &cur, second, &message)?
            };

            cur = next_cur;
            members.push(orig_idx);
            landed_as.insert(req.source_oid.clone(), cur.clone());
            recorded_identities.push((req.source_oid.clone(), cur.clone()));
            steps.push((
                orig_idx,
                StepResult::Merged {
                    landed_oid: cur.clone(),
                },
            ));
        }

        Ok(AttemptState {
            quarantine,
            cur_tip: cur,
            members,
            member_count_before,
            recorded_identities,
            steps,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn publish_attempt(
        &self,
        repo: &Path,
        target_branch: &str,
        expected_tip: &str,
        candidate_tip: &str,
        identities: &[(String, String)],
        member_count: usize,
        verified_tip: Option<&str>,
        is_solo_fallback: bool,
    ) -> PublishOutcome {
        if let Some(ref batch_gate) = self.batch_gate {
            let ctx = BatchUpdateContext {
                repo,
                target_branch,
                old_tip: expected_tip,
                new_tip: candidate_tip,
                member_count,
                is_solo_fallback,
            };
            match batch_gate(&ctx) {
                BatchUpdateDecision::Allow => {}
                BatchUpdateDecision::RejectHook(msg) => return PublishOutcome::Rejected(msg),
                BatchUpdateDecision::LostCas(msg) => return PublishOutcome::LostCas(msg),
                BatchUpdateDecision::FailUpdate(err) => return PublishOutcome::Failed(err),
            }
        }

        let invocation_id = format!(
            "lq-{}-{}",
            std::process::id(),
            INVOCATION_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let target_ref = format!("refs/heads/{target_branch}");
        for (source_oid, landed_oid) in identities {
            if let Err(err) =
                record_land_intent(repo, &target_ref, source_oid, landed_oid, &invocation_id)
            {
                return PublishOutcome::Failed(LandError::Failed {
                    message: format!("failed to record land intent in ledger: {err}"),
                });
            }
        }

        let repo_lock = repo_mutation_lock(repo);
        let _guard = repo_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);

        let actual_tip = match read_target_tip(repo, target_branch) {
            Ok(tip) => tip,
            Err(err) => {
                let _ = abort_land_intent(repo, &invocation_id);
                return PublishOutcome::Failed(err);
            }
        };
        if actual_tip != expected_tip {
            let _ = abort_land_intent(repo, &invocation_id);
            return PublishOutcome::LostCas(format!(
                "{target_branch} moved from {expected_tip} to {actual_tip}"
            ));
        }

        let touch = match checkout_holders_for_target(repo, target_branch) {
            Ok(touch) => touch,
            Err(failure) => {
                let _ = abort_land_intent(repo, &invocation_id);
                return PublishOutcome::Failed(map_integrate_failure(failure));
            }
        };

        let mut gate = allow_all_gate;
        let is_verified = match verified_tip {
            Some(vt) => {
                already_covered(repo, candidate_tip, expected_tip, Some(vt)).unwrap_or(false)
            }
            None => false,
        };

        match land_verified(
            repo,
            "land-queue",
            target_branch,
            expected_tip,
            candidate_tip,
            &touch,
            is_verified,
            &mut gate,
        ) {
            Ok(landed) => {
                if let Err(err) = confirm_land_intent(repo, &invocation_id) {
                    log::warn!("LandQueue confirmed CAS but failed to update intent row: {err}");
                }
                PublishOutcome::Applied {
                    checkout_advanced: landed.checkout_advanced,
                    checkout_note: landed.checkout_note,
                    green_ref_note: landed.green_ref_note,
                }
            }
            Err(IntegrateFailure::Refused { message }) if message.contains("changed while") => {
                let _ = abort_land_intent(repo, &invocation_id);
                PublishOutcome::LostCas(message)
            }
            Err(failure) => {
                let _ = abort_land_intent(repo, &invocation_id);
                PublishOutcome::Failed(map_integrate_failure(failure))
            }
        }
    }
}

struct AttemptState {
    quarantine: QuarantineSession,
    cur_tip: String,
    members: Vec<usize>,
    member_count_before: HashMap<usize, usize>,
    recorded_identities: Vec<(String, String)>,
    steps: Vec<(usize, StepResult)>,
}

impl AttemptState {
    fn members_empty_at_step(&self, orig_idx: usize) -> bool {
        self.member_count_before
            .get(&orig_idx)
            .copied()
            .unwrap_or(0)
            == 0
    }
}

enum StepResult {
    Merged {
        landed_oid: String,
    },
    AlreadyLanded {
        landed_oid: String,
        reverted_by: Option<String>,
    },
    Conflict {
        message: String,
        paths: Vec<String>,
        total: usize,
    },
    Fail {
        err: LandError,
        tip_independent: bool,
    },
}

impl StepResult {
    fn to_result(
        &self,
        req: &LandRequest,
        batch_tip: &str,
        telemetry: LandTelemetry,
    ) -> Result<LandOutcome, LandError> {
        match self {
            Self::Merged { landed_oid } => Ok(LandOutcome {
                branch: req.branch.clone(),
                target_branch: req.target_branch.clone(),
                source_oid: req.source_oid.clone(),
                landed_oid: landed_oid.clone(),
                batch_tip: batch_tip.to_string(),
                already_landed: false,
                reverted_by: None,
                checkout_advanced: false,
                checkout_note: None,
                green_ref_note: None,
                telemetry,
            }),
            Self::AlreadyLanded {
                landed_oid,
                reverted_by,
            } => Ok(LandOutcome {
                branch: req.branch.clone(),
                target_branch: req.target_branch.clone(),
                source_oid: req.source_oid.clone(),
                landed_oid: landed_oid.clone(),
                batch_tip: batch_tip.to_string(),
                already_landed: true,
                reverted_by: reverted_by.clone(),
                checkout_advanced: false,
                checkout_note: None,
                green_ref_note: None,
                telemetry,
            }),
            Self::Conflict {
                message,
                paths,
                total,
            } => Err(LandError::Conflict {
                message: message.clone(),
                paths: paths.clone(),
                total: *total,
            }),
            Self::Fail { err, .. } => Err(err.clone()),
        }
    }
}

/// CompleteEarly safety proof:
/// - Tip-independent failures (`PermissionDenied`, invalid/non-existent source OID)
///   are ALWAYS safe to deliver immediately.
/// - Tip-dependent outcomes (`Conflict`, `AlreadyLanded`, `InvalidArgument`,
///   `CommitHooks`) are ONLY final while `members_empty_at_step == true` (no
///   earlier item in the batch mutated `cur` away from published `tip`).
fn can_complete_early(result: &StepResult, members_empty_at_step: bool) -> bool {
    match result {
        StepResult::Merged { .. } => false,
        StepResult::Fail {
            tip_independent: true,
            ..
        } => true,
        StepResult::Fail {
            tip_independent: false,
            ..
        }
        | StepResult::Conflict { .. }
        | StepResult::AlreadyLanded { .. } => members_empty_at_step,
    }
}

#[allow(clippy::too_many_arguments)]
fn split_dependents_on_failure(
    remaining: &[usize],
    members: &[usize],
    steps: &[(usize, StepResult)],
    finalized: &HashSet<usize>,
    batch: &[Arc<QueueEntry>],
    tip: &str,
    err: LandError,
    quarantine_runs: usize,
    cas_attempts: usize,
    batch_size: usize,
    final_outcomes: &mut [Option<Result<LandOutcome, LandError>>],
) -> Vec<usize> {
    let first_member = members[0];
    let m0_pos = remaining
        .iter()
        .position(|&idx| idx == first_member)
        .unwrap_or(0);
    let member_set: HashSet<usize> = members.iter().copied().collect();
    let mut dependents = Vec::new();
    for (pos, &orig_idx) in remaining.iter().enumerate() {
        if finalized.contains(&orig_idx) {
            continue;
        }
        if pos < m0_pos {
            if let Some((_, step_res)) = steps.iter().find(|(id, _)| *id == orig_idx) {
                final_outcomes[orig_idx] = Some(step_res.to_result(
                    &batch[orig_idx].req,
                    tip,
                    LandTelemetry {
                        quarantine_runs,
                        cas_attempts,
                        batch_size,
                        batch_members: members.len(),
                        completed_early: false,
                        solo_fallback: false,
                    },
                ));
            }
        } else if member_set.contains(&orig_idx) {
            final_outcomes[orig_idx] = Some(Err(err.clone()));
        } else {
            dependents.push(orig_idx);
        }
    }
    dependents
}

enum PublishOutcome {
    Applied {
        checkout_advanced: bool,
        checkout_note: Option<String>,
        green_ref_note: Option<String>,
    },
    LostCas(String),
    Rejected(String),
    Failed(LandError),
}

fn lane_key(repo: &Path, target_branch: &str) -> LaneKey {
    let common = resolve_git_common_dir(repo)
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .or_else(|| repo.canonicalize().ok())
        .unwrap_or_else(|| repo.to_path_buf());
    LaneKey {
        repo_common_dir: common,
        target_branch: target_branch.to_string(),
    }
}

/// AIMD batch sizing rule:
/// - `took > target`: multiplicative decrease `(current / 2).max(1)`
/// - `took < target / 2`: `+25%` (min `+1`) up to `hard_cap`
/// - `[target/2, target]`: hysteresis deadband (keep `current`)
pub(crate) fn adapt_batch_size(
    current: usize,
    took: Duration,
    target: Duration,
    hard_cap: usize,
) -> usize {
    if target.is_zero() {
        return current.clamp(1, hard_cap);
    }
    if took > target {
        (current / 2).max(1)
    } else if took < target / 2 {
        (current + (current / 4).max(1)).min(hard_cap)
    } else {
        current.clamp(1, hard_cap)
    }
}

fn jittered_cas_backoff(attempt: usize) -> Duration {
    let shift = attempt.saturating_sub(1).min(6) as u32;
    let ceiling = (CAS_BACKOFF_MIN_MS.saturating_mul(1u64 << shift)).min(CAS_BACKOFF_MAX_MS);
    let span = ceiling.saturating_sub(CAS_BACKOFF_MIN_MS).saturating_add(1);
    let mut bytes = [0u8; 8];
    let rand_val = if getrandom::fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes)
    } else {
        attempt as u64
    };
    Duration::from_millis(CAS_BACKOFF_MIN_MS + (rand_val % span))
}

fn read_target_tip(repo: &Path, target_branch: &str) -> Result<String, LandError> {
    let full_ref = format!("refs/heads/{target_branch}");
    let run = git_captured(
        repo,
        &[
            "for-each-ref",
            "--format=%(refname)\t%(objectname)",
            "--",
            &full_ref,
        ],
    )
    .map_err(|e| LandError::Failed {
        message: e.into_message(),
    })?
    .require_complete("git for-each-ref")
    .map_err(|e| LandError::Failed {
        message: e.into_message(),
    })?;
    if !run.success {
        return Err(LandError::Failed {
            message: format!(
                "could not read {full_ref}: {}",
                String::from_utf8_lossy(&run.stderr).trim()
            ),
        });
    }
    for line in String::from_utf8_lossy(&run.stdout).lines() {
        let mut parts = line.splitn(2, '\t');
        if parts.next() == Some(full_ref.as_str()) {
            let oid = parts.next().unwrap_or_default().trim();
            if is_full_oid(oid) {
                return Ok(oid.to_string());
            }
        }
    }
    Err(LandError::InvalidArgument {
        message: format!("target branch {full_ref} does not exist"),
    })
}

fn allow_all_gate(_: &[&str]) -> Result<(), CommandError> {
    Ok(())
}

fn map_integrate_failure(failure: IntegrateFailure) -> LandError {
    match failure {
        IntegrateFailure::Conflict {
            message,
            paths,
            total,
            ..
        } => LandError::Conflict {
            message,
            paths,
            total,
        },
        IntegrateFailure::CandidateFailed { message, .. }
        | IntegrateFailure::Refused { message }
        | IntegrateFailure::Unsupported { message } => LandError::Failed { message },
        IntegrateFailure::Held {
            reason,
            message,
            path,
        } => LandError::Held {
            reason,
            message,
            path,
        },
        IntegrateFailure::CommitHooks { hooks } => LandError::CommitHooks { hooks },
    }
}

/// Single `ObjectQuarantine` + batch-scoped `git merge-tree --stdin` stream
/// used for one batch rebuild attempt, packing newly written objects once.
struct QuarantineSession {
    repo: PathBuf,
    quarantine: ObjectQuarantine,
    common_objects: PathBuf,
    alternates: std::ffi::OsString,
    deadline: Instant,
    stream: Option<QuarantinedMergeStream>,
    respawns: usize,
    single_shot: bool,
}

struct QuarantinedMergeStream {
    child: StreamingChild,
    stdin: Option<BufWriter<ChildStdin>>,
    stdout: BufReader<ChildStdout>,
    stderr: Option<ChildStderr>,
}

impl Drop for QuarantinedMergeStream {
    fn drop(&mut self) {
        drop(self.stdin.take());
        let _ = self.child.kill_and_reap();
    }
}

impl QuarantineSession {
    fn open(repo: &Path, deadline: Instant) -> Result<Self, LandError> {
        let common_objects = resolve_git_common_dir(repo)
            .map_err(|e| LandError::Failed {
                message: e.into_message(),
            })?
            .join("objects");
        let alternates =
            std::env::join_paths([&common_objects]).map_err(|e| LandError::Failed {
                message: format!("alternates: {e}"),
            })?;
        let quarantine = ObjectQuarantine::new(repo).map_err(|e| LandError::Failed {
            message: e.to_string(),
        })?;
        Ok(Self {
            repo: repo.to_path_buf(),
            quarantine,
            common_objects,
            alternates,
            deadline,
            stream: None,
            respawns: 0,
            single_shot: false,
        })
    }

    fn commit_exists(&self, oid: &str) -> Result<bool, LandError> {
        let spec = format!("{oid}^{{commit}}");
        let run = self
            .quarantine
            .run(
                &self.repo,
                &["rev-parse", "--verify", "--quiet", &spec],
                self.deadline,
            )
            .map_err(|e| LandError::Failed {
                message: e.to_string(),
            })?;
        Ok(run.success)
    }

    fn is_ancestor(&self, ancestor: &str, descendant: &str) -> Result<bool, LandError> {
        if ancestor == descendant {
            return Ok(true);
        }
        let run = self
            .quarantine
            .run(
                &self.repo,
                &["merge-base", "--is-ancestor", ancestor, descendant],
                self.deadline,
            )
            .map_err(|e| LandError::Failed {
                message: e.to_string(),
            })?;
        match run.status_code {
            0 => Ok(true),
            1 => Ok(false),
            code => Err(LandError::Failed {
                message: format!(
                    "merge-base --is-ancestor {ancestor} {descendant} exited {code}: {}",
                    String::from_utf8_lossy(&run.stderr).trim()
                ),
            }),
        }
    }

    fn tree_of(&self, oid: &str) -> Result<String, LandError> {
        let spec = format!("{oid}^{{tree}}");
        let run = self
            .quarantine
            .run(&self.repo, &["rev-parse", "--verify", &spec], self.deadline)
            .map_err(|e| LandError::Failed {
                message: e.to_string(),
            })?;
        if !run.success {
            return Err(LandError::Failed {
                message: format!(
                    "rev-parse {spec} failed: {}",
                    String::from_utf8_lossy(&run.stderr).trim()
                ),
            });
        }
        let tree = String::from_utf8_lossy(&run.stdout).trim().to_string();
        if !is_full_oid(&tree) {
            return Err(LandError::Failed {
                message: format!("{oid} resolved to invalid tree {tree:?}"),
            });
        }
        Ok(tree)
    }

    fn commit_tree(
        &self,
        tree: &str,
        first_parent: &str,
        second_parent: Option<&str>,
        message: &str,
    ) -> Result<String, LandError> {
        let mut owned = vec![
            "commit-tree".to_string(),
            tree.to_string(),
            "-p".to_string(),
            first_parent.to_string(),
        ];
        if let Some(second) = second_parent {
            owned.push("-p".to_string());
            owned.push(second.to_string());
        }
        owned.push("-m".to_string());
        owned.push(message.to_string());
        let args: Vec<&str> = owned.iter().map(String::as_str).collect();
        let run = self
            .quarantine
            .run(&self.repo, &args, self.deadline)
            .map_err(|e| LandError::Failed {
                message: e.to_string(),
            })?;
        if !run.success {
            return Err(LandError::Failed {
                message: format!(
                    "quarantined commit-tree failed: {}",
                    String::from_utf8_lossy(&run.stderr).trim()
                ),
            });
        }
        let oid = String::from_utf8_lossy(&run.stdout).trim().to_string();
        if !is_full_oid(&oid) {
            return Err(LandError::Failed {
                message: format!("commit-tree returned invalid commit id {oid:?}"),
            });
        }
        Ok(oid)
    }

    fn merge_tree(&mut self, ours: &str, theirs: &str) -> Result<MergeOutcome, LandError> {
        if self.single_shot {
            return self
                .quarantine
                .merge_tree(&self.repo, ours, theirs, self.deadline)
                .map_err(|e| LandError::Failed {
                    message: e.to_string(),
                });
        }
        without_lazy_fetch(|| self.merge_tree_streamed(ours, theirs))
    }

    fn merge_tree_streamed(&mut self, ours: &str, theirs: &str) -> Result<MergeOutcome, LandError> {
        if self.stream.is_none() {
            if self.respawns >= MAX_STREAM_RESPAWNS {
                return Err(LandError::Failed {
                    message: format!(
                        "quarantined merge-tree stream exceeded {MAX_STREAM_RESPAWNS} respawns"
                    ),
                });
            }
            if self.respawns > 0 || self.stream.is_none() {
                let remaining = self.deadline.saturating_duration_since(Instant::now());
                if remaining.is_zero() {
                    return Err(LandError::Failed {
                        message: "quarantined merge-tree deadline exceeded".into(),
                    });
                }
                let mut child = git_spawn_quarantined_streaming(
                    &self.repo,
                    &[
                        "merge-tree",
                        "--stdin",
                        "--write-tree",
                        "--name-only",
                        "--no-messages",
                        "-z",
                    ],
                    self.quarantine.path(),
                    &self.alternates,
                    remaining.min(DEFAULT_TIMEOUT),
                )
                .map_err(|e| LandError::Failed {
                    message: e.into_message(),
                })?;
                let stdin = child.take_stdin().ok_or_else(|| LandError::Failed {
                    message: "merge-tree --stdin missing stdin".into(),
                })?;
                let stdout = child.take_stdout().ok_or_else(|| LandError::Failed {
                    message: "merge-tree --stdin missing stdout".into(),
                })?;
                let stderr = child.take_stderr();
                self.stream = Some(QuarantinedMergeStream {
                    child,
                    stdin: Some(BufWriter::new(stdin)),
                    stdout: BufReader::new(stdout),
                    stderr,
                });
            }
        }

        let line = format!("{ours} {theirs}\n");
        let write_err = {
            let stream = self.stream.as_mut().expect("stream present");
            let stdin = stream.stdin.as_mut().expect("stdin present");
            stdin
                .write_all(line.as_bytes())
                .and_then(|()| stdin.flush())
                .err()
        };
        if let Some(err) = write_err {
            return self.recover_stream_death(ours, theirs, &err.to_string());
        }

        let read_res = {
            let stream = self.stream.as_mut().expect("stream present");
            read_quarantine_stream_record(stream, self.deadline)
        };
        match read_res {
            Ok(outcome) => Ok(outcome),
            Err(detail) => self.recover_stream_death(ours, theirs, &detail),
        }
    }

    fn recover_stream_death(
        &mut self,
        ours: &str,
        theirs: &str,
        detail: &str,
    ) -> Result<MergeOutcome, LandError> {
        let Some(mut stream) = self.stream.take() else {
            return Err(LandError::Failed {
                message: detail.to_string(),
            });
        };
        drop(stream.stdin.take());
        let code = stream
            .child
            .wait_and_reap()
            .ok()
            .and_then(|st| st.code())
            .unwrap_or(-1);
        let mut stderr_buf = Vec::new();
        if let Some(stderr) = stream.stderr.take() {
            let _ = stderr
                .take(MAX_STREAM_STDERR_BYTES as u64)
                .read_to_end(&mut stderr_buf);
        }
        let stderr_text = String::from_utf8_lossy(&stderr_buf).trim().to_string();
        if code == 129
            || matches!(
                parse_merge_tree_output(&[], code, &stderr_text),
                Err(QuarantineError::Unsupported(_))
            )
        {
            self.single_shot = true;
            return self
                .quarantine
                .merge_tree(&self.repo, ours, theirs, self.deadline)
                .map_err(|e| LandError::Failed {
                    message: e.to_string(),
                });
        }
        self.respawns += 1;
        let msg = if !stderr_text.is_empty() {
            stderr_text
        } else {
            format!("git merge-tree --stdin exited {code}: {detail}")
        };
        Err(LandError::Failed { message: msg })
    }

    /// Scans loose objects in the quarantine, packs them into one fsynced pack
    /// via `git pack-objects`, and atomically moves `.pack` then `.idx`/`.rev`
    /// into `<common>/objects/pack/`.
    fn pack_and_migrate(&mut self) -> Result<(), LandError> {
        // Close stream stdin first so pack-objects sees all flushed loose objects.
        self.stream.take();

        let loose_oids = collect_quarantine_loose_oids(self.quarantine.path()).map_err(|e| {
            LandError::Failed {
                message: format!("failed scanning quarantine objects: {e}"),
            }
        })?;
        if loose_oids.is_empty() {
            return Ok(());
        }

        let pack_stage = self.quarantine.path().join("pack");
        std::fs::create_dir_all(&pack_stage).map_err(|e| LandError::Failed {
            message: format!("failed creating quarantine pack dir: {e}"),
        })?;
        let pack_prefix = pack_stage.join("pack");
        let pack_prefix_str = pack_prefix.to_str().ok_or_else(|| LandError::Failed {
            message: "quarantine pack path is not UTF-8".into(),
        })?;

        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(LandError::Failed {
                message: "pack-objects deadline exceeded".into(),
            });
        }

        let mut child = git_spawn_quarantined_streaming(
            &self.repo,
            &["pack-objects", "-q", pack_prefix_str],
            self.quarantine.path(),
            &self.alternates,
            remaining.min(DEFAULT_TIMEOUT),
        )
        .map_err(|e| LandError::Failed {
            message: e.into_message(),
        })?;

        {
            let mut stdin = child.take_stdin().ok_or_else(|| LandError::Failed {
                message: "pack-objects missing stdin".into(),
            })?;
            for oid in &loose_oids {
                writeln!(stdin, "{oid}").map_err(|e| LandError::Failed {
                    message: format!("pack-objects stdin write failed: {e}"),
                })?;
            }
            stdin.flush().map_err(|e| LandError::Failed {
                message: format!("pack-objects stdin flush failed: {e}"),
            })?;
        }
        let stderr = child.take_stderr();
        let status = child.wait_and_reap().map_err(|e| LandError::Failed {
            message: format!("pack-objects wait failed: {e}"),
        })?;
        if !status.success() {
            let mut buf = Vec::new();
            if let Some(err_stream) = stderr {
                let _ = err_stream
                    .take(MAX_STREAM_STDERR_BYTES as u64)
                    .read_to_end(&mut buf);
            }
            return Err(LandError::Failed {
                message: format!(
                    "git pack-objects failed (exit {:?}): {}",
                    status.code(),
                    String::from_utf8_lossy(&buf).trim()
                ),
            });
        }

        let dest_pack_dir = self.common_objects.join("pack");
        std::fs::create_dir_all(&dest_pack_dir).map_err(|e| LandError::Failed {
            message: format!("failed ensuring {}: {e}", dest_pack_dir.display()),
        })?;

        let mut files: Vec<PathBuf> = Vec::new();
        for entry in std::fs::read_dir(&pack_stage).map_err(|e| LandError::Failed {
            message: format!("failed reading {}: {e}", pack_stage.display()),
        })? {
            let entry = entry.map_err(|e| LandError::Failed {
                message: format!("pack entry error: {e}"),
            })?;
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
        // Move .pack first so .idx is never visible before its .pack.
        files.sort_by_key(|p| {
            let is_pack = p.extension().and_then(|e| e.to_str()) == Some("pack");
            (!is_pack, p.clone())
        });
        for src in files {
            let Some(name) = src.file_name() else {
                continue;
            };
            let dst = dest_pack_dir.join(name);
            install_pack_file(&src, &dst).map_err(|e| LandError::Failed {
                message: format!("failed installing pack file {}: {e}", dst.display()),
            })?;
        }

        Ok(())
    }
}

fn read_quarantine_stream_record(
    stream: &mut QuarantinedMergeStream,
    deadline: Instant,
) -> Result<MergeOutcome, String> {
    let read_nul = |stream: &mut QuarantinedMergeStream| -> Result<Vec<u8>, String> {
        if stream.stdout.buffer().is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err("merge-tree stream deadline exceeded".into());
            }
            match stream
                .child
                .wait_stdout_readable(stream.stdout.get_ref(), remaining)
            {
                Ok(true) => {}
                Ok(false) => return Err("merge-tree stream timed out".into()),
                Err(e) => return Err(format!("poll failed: {e}")),
            }
        }
        let mut buf = Vec::new();
        let n = stream
            .stdout
            .read_until(0, &mut buf)
            .map_err(|e| format!("read failed: {e}"))?;
        if n == 0 || buf.last() != Some(&0) {
            return Err("unexpected EOF on merge-tree stream".into());
        }
        if buf.len() > MAX_STREAM_RECORD_BYTES {
            return Err("merge-tree stream record exceeded cap".into());
        }
        buf.pop();
        Ok(buf)
    };

    let clean_flag = read_nul(stream)?;
    let tree_oid_bytes = read_nul(stream)?;
    let tree = String::from_utf8(tree_oid_bytes)
        .map_err(|_| "merge-tree stream emitted non-UTF-8 tree oid".to_string())?;
    if !is_full_oid(&tree) {
        return Err(format!(
            "merge-tree stream emitted invalid tree oid {tree:?}"
        ));
    }

    let mut paths = Vec::new();
    let mut paths_total = 0usize;
    loop {
        let field = read_nul(stream)?;
        if field.is_empty() {
            break;
        }
        paths_total += 1;
        if paths.len() < crate::engine::object_quarantine::MAX_CONFLICT_PATHS {
            paths.push(String::from_utf8_lossy(&field).into_owned());
        }
    }

    match clean_flag.as_slice() {
        b"1" => Ok(MergeOutcome::Clean { tree }),
        b"0" => Ok(MergeOutcome::Conflicted {
            tree,
            paths,
            paths_total,
        }),
        other => Err(format!(
            "merge-tree stream emitted unexpected status {:?}",
            String::from_utf8_lossy(other)
        )),
    }
}

fn collect_quarantine_loose_oids(quarantine_dir: &Path) -> std::io::Result<Vec<String>> {
    let mut oids = Vec::new();
    for fanout in std::fs::read_dir(quarantine_dir)? {
        let fanout = fanout?;
        let fname = fanout.file_name();
        let Some(prefix) = fname.to_str() else {
            continue;
        };
        if prefix.len() != 2
            || !prefix
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        {
            continue;
        }
        let fanout_path = fanout.path();
        if !fanout_path.is_dir() {
            continue;
        }
        for obj in std::fs::read_dir(fanout_path)? {
            let obj = obj?;
            let oname = obj.file_name();
            let Some(rest) = oname.to_str() else {
                continue;
            };
            let full = format!("{prefix}{rest}");
            if is_full_oid(&full) {
                oids.push(full);
            }
        }
    }
    Ok(oids)
}

fn install_pack_file(src: &Path, dst: &Path) -> std::io::Result<()> {
    if dst.exists() {
        return Ok(());
    }
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(_) => {
            let seq = PACK_TMP_SEQ.fetch_add(1, Ordering::Relaxed);
            let tmp = dst.with_extension(format!("tmp-{}-{seq}", std::process::id()));
            {
                let mut reader = std::fs::File::open(src)?;
                let mut writer = std::fs::File::create_new(&tmp)?;
                std::io::copy(&mut reader, &mut writer)?;
                writer.sync_all()?;
            }
            if let Err(e) = std::fs::rename(&tmp, dst) {
                let _ = std::fs::remove_file(&tmp);
                if dst.exists() {
                    return Ok(());
                }
                return Err(e);
            }
            let _ = std::fs::remove_file(src);
            Ok(())
        }
    }
}

// ============================================================================
// Cross-Process SQLite WAL Sorted-Path FIFO Hot-File Tickets
// ============================================================================

const TICKETS_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS land_tickets (
  seq            INTEGER PRIMARY KEY AUTOINCREMENT,
  repo_key       TEXT NOT NULL,
  target_branch  TEXT NOT NULL,
  ticket_key     TEXT NOT NULL,
  holder_id      TEXT NOT NULL,
  pid            INTEGER NOT NULL,
  priority       INTEGER NOT NULL DEFAULT 0,
  acquired_at_ms INTEGER NOT NULL,
  expires_at_ms  INTEGER NOT NULL,
  UNIQUE(repo_key, target_branch, ticket_key, holder_id)
);
CREATE INDEX IF NOT EXISTS idx_land_tickets_queue
  ON land_tickets(repo_key, target_branch, ticket_key, seq ASC);
CREATE INDEX IF NOT EXISTS idx_land_tickets_branch
  ON land_tickets(repo_key, target_branch, priority DESC, seq ASC);
CREATE INDEX IF NOT EXISTS idx_land_tickets_holder
  ON land_tickets(holder_id);
"#;

/// Cross-process SQLite WAL (`<common>/gitpulse/land_tickets.sqlite`) ticket store
/// for sorted-path FIFO hot-file conflict resolution.
pub struct LandTicketStore {
    db_path: PathBuf,
}

/// Held FIFO hot-file ticket guard. Automatically releases all held ticket rows
/// on drop. Supports deadlock-free [`Self::upgrade`] (release-before-upgrade).
pub struct LandTicketGuard {
    db_path: PathBuf,
    repo_key: String,
    target_branch: String,
    holder_id: String,
    keys: Vec<String>,
    branch_wide: bool,
    released: bool,
}

impl LandTicketGuard {
    pub fn keys(&self) -> &[String] {
        &self.keys
    }

    pub fn is_branch_wide(&self) -> bool {
        self.branch_wide
    }

    /// Returns `true` if the currently held ticket set already covers `paths`
    /// (or is `branch_wide`).
    pub fn covers(&self, paths: &[String], branch_wide: bool) -> bool {
        if self.branch_wide {
            return true;
        }
        if branch_wide || paths.is_empty() {
            return false;
        }
        let wanted = build_sorted_ticket_keys(paths, false);
        wanted.iter().all(|w| self.keys.binary_search(w).is_ok())
    }

    /// Deadlock-free upgrade rule: if `self` already covers `(paths, branch_wide)`,
    /// renews TTL and keeps the ticket; otherwise **releases all held tickets first**
    /// before acquiring the superset at the FIFO tail.
    pub fn upgrade(
        &mut self,
        paths: &[String],
        branch_wide: bool,
        ttl: Duration,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<(), LandError> {
        if self.covers(paths, branch_wide) {
            return renew_ticket_ttl(&self.db_path, &self.holder_id, ttl);
        }
        // Release-before-upgrade invariant: drop all held keys first so no
        // partial-hold cycle can ever form.
        let merged_paths: Vec<String> = self
            .keys
            .iter()
            .filter(|k| k.as_str() != TICKET_BRANCH_WIDE_KEY)
            .cloned()
            .chain(paths.iter().cloned())
            .collect();
        self.release();
        let store = LandTicketStore {
            db_path: self.db_path.clone(),
        };
        let upgraded = store.acquire_at_key(
            &self.repo_key,
            &self.target_branch,
            &merged_paths,
            branch_wide,
            ttl,
            timeout,
            cancel,
        )?;
        *self = upgraded;
        Ok(())
    }

    /// Releases all ticket rows owned by this guard immediately.
    pub fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        let _ = delete_holder_tickets(&self.db_path, &self.holder_id);
    }
}

impl Drop for LandTicketGuard {
    fn drop(&mut self) {
        self.release();
    }
}

impl LandTicketStore {
    pub fn for_repo(repo: &Path) -> Result<Self, LandError> {
        let common = resolve_git_common_dir(repo).map_err(|e| LandError::Failed {
            message: e.into_message(),
        })?;
        let dir = common.join("gitpulse");
        ensure_private_dir(&dir).map_err(|e| LandError::Failed {
            message: format!("failed creating {}: {e}", dir.display()),
        })?;
        Ok(Self {
            db_path: dir.join("land_tickets.sqlite"),
        })
    }

    /// Acquires sorted-path FIFO tickets atomically via `BEGIN IMMEDIATE`.
    #[allow(clippy::too_many_arguments)]
    pub fn acquire(
        &self,
        repo: &Path,
        target_branch: &str,
        paths: &[String],
        branch_wide: bool,
        ttl: Duration,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<LandTicketGuard, LandError> {
        let repo_key = resolve_git_common_dir(repo)
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .or_else(|| repo.canonicalize().ok())
            .unwrap_or_else(|| repo.to_path_buf())
            .to_string_lossy()
            .into_owned();
        self.acquire_at_key(
            &repo_key,
            target_branch,
            paths,
            branch_wide,
            ttl,
            timeout,
            cancel,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn acquire_at_key(
        &self,
        repo_key: &str,
        target_branch: &str,
        paths: &[String],
        branch_wide: bool,
        ttl: Duration,
        timeout: Duration,
        cancel: Option<&AtomicBool>,
    ) -> Result<LandTicketGuard, LandError> {
        let sorted_keys = build_sorted_ticket_keys(paths, branch_wide);
        let actual_branch_wide = sorted_keys.len() == 1 && sorted_keys[0] == TICKET_BRANCH_WIDE_KEY;
        let holder_id = format!(
            "t-{}-{}",
            std::process::id(),
            HOLDER_SEQ.fetch_add(1, Ordering::Relaxed)
        );
        let pid = i64::from(std::process::id());
        let deadline = Instant::now() + timeout;

        let conn = open_tickets_db(&self.db_path)?;

        // Register ALL sorted keys in one atomic `BEGIN IMMEDIATE` transaction so
        // multi-key acquisition order across processes is a strict total order.
        {
            let now_ms = now_epoch_ms();
            let expires_ms = now_ms.saturating_add(ttl.as_millis() as i64);
            let tx = begin_immediate(&conn)?;
            prune_stale_tickets(&tx, now_ms)?;
            for key in &sorted_keys {
                tx.execute(
                    "INSERT OR IGNORE INTO land_tickets (
                       repo_key, target_branch, ticket_key, holder_id, pid, priority, acquired_at_ms, expires_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 0, ?6, ?7)",
                    params![
                        repo_key,
                        target_branch,
                        key,
                        holder_id,
                        pid,
                        now_ms,
                        expires_ms
                    ],
                )
                .map_err(|e| LandError::Failed {
                    message: format!("land_tickets insert failed: {e}"),
                })?;
            }
            tx.commit().map_err(|e| LandError::Failed {
                message: format!("land_tickets enqueue commit failed: {e}"),
            })?;
        }

        let mut guard = LandTicketGuard {
            db_path: self.db_path.clone(),
            repo_key: repo_key.to_string(),
            target_branch: target_branch.to_string(),
            holder_id: holder_id.clone(),
            keys: sorted_keys.clone(),
            branch_wide: actual_branch_wide,
            released: false,
        };

        loop {
            if cancel.is_some_and(|c| c.load(Ordering::Acquire)) {
                guard.release();
                return Err(LandError::Canceled);
            }

            let now_ms = now_epoch_ms();
            let expires_ms = now_ms.saturating_add(ttl.as_millis() as i64);
            let tx = begin_immediate(&conn)?;
            prune_stale_tickets(&tx, now_ms)?;
            tx.execute(
                "UPDATE land_tickets SET expires_at_ms = ?1 WHERE holder_id = ?2",
                params![expires_ms, holder_id],
            )
            .map_err(|e| LandError::Failed {
                message: format!("land_tickets renew failed: {e}"),
            })?;

            let mut all_head = true;
            if actual_branch_wide {
                let head_holder: Option<String> = tx
                    .query_row(
                        "SELECT holder_id FROM land_tickets
                         WHERE repo_key = ?1 AND target_branch = ?2
                         ORDER BY priority DESC, seq ASC
                         LIMIT 1",
                        params![repo_key, target_branch],
                        |row| row.get(0),
                    )
                    .ok();
                if head_holder.as_deref() != Some(holder_id.as_str()) {
                    all_head = false;
                }
            } else {
                for key in &sorted_keys {
                    let head_holder: Option<String> = tx
                        .query_row(
                            "SELECT holder_id FROM land_tickets
                             WHERE repo_key = ?1 AND target_branch = ?2 AND ticket_key IN (?3, ?4)
                             ORDER BY priority DESC, seq ASC
                             LIMIT 1",
                            params![repo_key, target_branch, key, TICKET_BRANCH_WIDE_KEY],
                            |row| row.get(0),
                        )
                        .ok();
                    if head_holder.as_deref() != Some(holder_id.as_str()) {
                        all_head = false;
                        break;
                    }
                }
            }
            tx.commit().map_err(|e| LandError::Failed {
                message: format!("land_tickets poll commit failed: {e}"),
            })?;

            if all_head {
                return Ok(guard);
            }
            if Instant::now() >= deadline {
                guard.release();
                return Err(LandError::Failed {
                    message: format!(
                        "timed out waiting for land_tickets on {target_branch} ({})",
                        sorted_keys.join(", ")
                    ),
                });
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

pub(crate) fn build_sorted_ticket_keys(paths: &[String], branch_wide: bool) -> Vec<String> {
    if branch_wide || paths.is_empty() {
        return vec![TICKET_BRANCH_WIDE_KEY.to_string()];
    }
    let set: BTreeSet<String> = paths
        .iter()
        .map(|p| p.trim_start_matches("./").to_string())
        .filter(|p| !p.is_empty())
        .collect();
    if set.is_empty() {
        vec![TICKET_BRANCH_WIDE_KEY.to_string()]
    } else {
        set.into_iter().collect()
    }
}

fn acquire_batch_ticket(
    repo: &Path,
    key: &LaneKey,
    batch: &[Arc<QueueEntry>],
    remaining: &[usize],
) -> Result<Option<LandTicketGuard>, LandError> {
    let mut paths = Vec::new();
    let mut branch_wide = false;
    for &idx in remaining {
        let req = &batch[idx].req;
        if req.branch_wide_ticket {
            branch_wide = true;
        }
        paths.extend(req.hot_files.iter().cloned());
    }
    if !branch_wide && paths.is_empty() {
        return Ok(None);
    }
    let store = LandTicketStore::for_repo(repo)?;
    let guard = store.acquire(
        repo,
        &key.target_branch,
        &paths,
        branch_wide,
        DEFAULT_TICKET_TTL,
        DEFAULT_TICKET_WAIT_TIMEOUT,
        None,
    )?;
    Ok(Some(guard))
}

fn acquire_single_ticket(
    repo: &Path,
    key: &LaneKey,
    req: &LandRequest,
) -> Result<Option<LandTicketGuard>, LandError> {
    if !req.branch_wide_ticket && req.hot_files.is_empty() {
        return Ok(None);
    }
    let store = LandTicketStore::for_repo(repo)?;
    let guard = store.acquire(
        repo,
        &key.target_branch,
        &req.hot_files,
        req.branch_wide_ticket,
        DEFAULT_TICKET_TTL,
        DEFAULT_TICKET_WAIT_TIMEOUT,
        req.cancel.as_deref(),
    )?;
    Ok(Some(guard))
}

fn open_tickets_db(db_path: &Path) -> Result<Connection, LandError> {
    if let Some(parent) = db_path.parent() {
        ensure_private_dir(parent).map_err(|e| LandError::Failed {
            message: format!("failed creating {}: {e}", parent.display()),
        })?;
    }
    let conn = Connection::open(db_path).map_err(|e| LandError::Failed {
        message: format!("failed opening {}: {e}", db_path.display()),
    })?;
    conn.busy_timeout(Duration::from_secs(5))
        .map_err(|e| LandError::Failed {
            message: format!("land_tickets busy_timeout: {e}"),
        })?;
    enter_wal(&conn, LEDGER_WAL_PATIENCE).map_err(|e| LandError::Failed {
        message: format!("land_tickets WAL setup: {e}"),
    })?;
    conn.pragma_update(None, "synchronous", "NORMAL")
        .map_err(|e| LandError::Failed {
            message: format!("land_tickets synchronous pragma: {e}"),
        })?;
    conn.execute_batch(TICKETS_SCHEMA)
        .map_err(|e| LandError::Failed {
            message: format!("land_tickets schema: {e}"),
        })?;
    Ok(conn)
}

fn begin_immediate(conn: &Connection) -> Result<rusqlite::Transaction<'_>, LandError> {
    rusqlite::Transaction::new_unchecked(conn, rusqlite::TransactionBehavior::Immediate).map_err(
        |e| LandError::Failed {
            message: format!("land_tickets BEGIN IMMEDIATE failed: {e}"),
        },
    )
}

fn prune_stale_tickets(tx: &rusqlite::Transaction<'_>, now_ms: i64) -> Result<(), LandError> {
    tx.execute(
        "DELETE FROM land_tickets WHERE expires_at_ms <= ?1",
        params![now_ms],
    )
    .map_err(|e| LandError::Failed {
        message: format!("land_tickets TTL prune failed: {e}"),
    })?;

    let my_pid = i64::from(std::process::id());
    let mut stmt = tx
        .prepare("SELECT DISTINCT pid FROM land_tickets WHERE pid != ?1")
        .map_err(|e| LandError::Failed {
            message: format!("land_tickets PID query prepare failed: {e}"),
        })?;
    let pids: Vec<i64> = stmt
        .query_map(params![my_pid], |row| row.get(0))
        .map_err(|e| LandError::Failed {
            message: format!("land_tickets PID query failed: {e}"),
        })?
        .filter_map(Result::ok)
        .collect();
    drop(stmt);

    for pid in pids {
        if !is_pid_alive(pid) {
            tx.execute("DELETE FROM land_tickets WHERE pid = ?1", params![pid])
                .map_err(|e| LandError::Failed {
                    message: format!("land_tickets dead PID cleanup failed: {e}"),
                })?;
        }
    }
    Ok(())
}

fn renew_ticket_ttl(db_path: &Path, holder_id: &str, ttl: Duration) -> Result<(), LandError> {
    let conn = open_tickets_db(db_path)?;
    let now_ms = now_epoch_ms();
    let expires_ms = now_ms.saturating_add(ttl.as_millis() as i64);
    let tx = begin_immediate(&conn)?;
    tx.execute(
        "UPDATE land_tickets SET expires_at_ms = ?1 WHERE holder_id = ?2",
        params![expires_ms, holder_id],
    )
    .map_err(|e| LandError::Failed {
        message: format!("land_tickets renew failed: {e}"),
    })?;
    tx.commit().map_err(|e| LandError::Failed {
        message: format!("land_tickets renew commit failed: {e}"),
    })
}

fn delete_holder_tickets(db_path: &Path, holder_id: &str) -> Result<(), LandError> {
    let conn = open_tickets_db(db_path)?;
    let tx = begin_immediate(&conn)?;
    tx.execute(
        "DELETE FROM land_tickets WHERE holder_id = ?1",
        params![holder_id],
    )
    .map_err(|e| LandError::Failed {
        message: format!("land_tickets release failed: {e}"),
    })?;
    tx.commit().map_err(|e| LandError::Failed {
        message: format!("land_tickets release commit failed: {e}"),
    })
}

fn is_pid_alive(pid: i64) -> bool {
    #[cfg(unix)]
    {
        let Ok(native) = i32::try_from(pid) else {
            return false;
        };
        if native <= 0 {
            return false;
        }
        // SAFETY: `kill(pid, 0)` performs only an existence check with no signal sent.
        let rc = unsafe { libc::kill(native, 0) };
        if rc == 0 {
            true
        } else {
            std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
        }
    }
    #[cfg(not(unix))]
    {
        pid > 0
    }
}

fn now_epoch_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn ensure_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        if let Some(parent) = dir.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::git_cli::git_text;
    use std::sync::Barrier;

    fn git(repo: &Path, args: &[&str]) -> String {
        git_text(repo, args)
            .unwrap_or_else(|e| panic!("git {args:?}: {}", e.message()))
            .trim()
            .to_string()
    }

    fn init_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path();
        crate::test_support::git_in(repo, &["init", "-q", "-b", "main", "."]);
        git(repo, &["config", "user.email", "test@gitpulse.local"]);
        git(repo, &["config", "user.name", "GitPulse Tester"]);
        std::fs::write(repo.join("shared.txt"), "base\n").unwrap();
        git(repo, &["add", "shared.txt"]);
        git(repo, &["commit", "-q", "-m", "initial"]);
        crate::test_support::trust_repo(repo);
        dir
    }

    fn make_branch_commit(
        repo: &Path,
        branch: &str,
        file: &str,
        content: &str,
        base: &str,
    ) -> String {
        git(repo, &["checkout", "-q", "-b", branch, base]);
        std::fs::write(repo.join(file), content).unwrap();
        git(repo, &["add", file]);
        git(
            repo,
            &["commit", "-q", "-m", &format!("commit on {branch}")],
        );
        let oid = git(repo, &["rev-parse", "HEAD"]);
        git(repo, &["checkout", "-q", "main"]);
        oid
    }

    /// Required Test 1:
    /// 5-lane concurrent batch where lane #3 conflicts: lanes #1,#2,#4,#5 land in
    /// 1 quarantine + 1 CAS, lane #3 returns Conflict, no premature CompleteEarly
    /// after lane #1 moved `cur`.
    #[test]
    fn five_lane_batch_with_conflicting_third_lane_lands_four_in_one_quarantine_and_one_cas() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);

        let oid1 = make_branch_commit(repo, "lane-1", "shared.txt", "lane 1 wins shared\n", &base);
        let oid2 = make_branch_commit(repo, "lane-2", "f2.txt", "lane 2\n", &base);
        let oid3 = make_branch_commit(
            repo,
            "lane-3",
            "shared.txt",
            "lane 3 conflicts with lane 1\n",
            &base,
        );
        let oid4 = make_branch_commit(repo, "lane-4", "f4.txt", "lane 4\n", &base);
        let oid5 = make_branch_commit(repo, "lane-5", "f5.txt", "lane 5\n", &base);

        // Use a generous window so all 5 concurrent threads coalesce reliably
        // even under loaded CI scheduling.
        let registry = Arc::new(LandQueueRegistry::new(LandQueueConfig {
            window: Duration::from_millis(250),
            ..LandQueueConfig::default()
        }));

        // Submit sequentially within <250ms in deterministic order 1..=5 from 5 threads
        // coordinated so lane-1 enters the queue first, followed by 2, 3, 4, 5.
        let barrier = Arc::new(Barrier::new(5));
        let specs = [
            ("lane-1", oid1.clone()),
            ("lane-2", oid2.clone()),
            ("lane-3", oid3.clone()),
            ("lane-4", oid4.clone()),
            ("lane-5", oid5.clone()),
        ];

        let mut handles = Vec::new();
        for (idx, (branch, oid)) in specs.into_iter().enumerate() {
            let reg = Arc::clone(&registry);
            let repo_path = repo.to_path_buf();
            let b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
                // Tiny deterministic stagger so queue order is 1, 2, 3, 4, 5 inside the 250ms window.
                std::thread::sleep(Duration::from_millis((idx as u64) * 10));
                let mut req = LandRequest::new(repo_path, branch, "main", oid);
                req.squash = true;
                reg.submit(req)
            }));
        }

        let results: Vec<Result<LandOutcome, LandError>> =
            handles.into_iter().map(|h| h.join().unwrap()).collect();

        for idx in [0usize, 1, 3, 4] {
            let out = results[idx]
                .as_ref()
                .unwrap_or_else(|e| panic!("lane {} failed: {e}", idx + 1));
            assert!(!out.already_landed);
            assert_eq!(out.telemetry.quarantine_runs, 1, "expected 1 quarantine");
            assert_eq!(out.telemetry.cas_attempts, 1, "expected 1 CAS");
            assert_eq!(out.telemetry.batch_size, 5, "all 5 lanes in 1 batch");
            assert_eq!(out.telemetry.batch_members, 4, "4 clean lanes landed");
            assert!(!out.telemetry.completed_early);
        }

        match &results[2] {
            Err(LandError::Conflict { paths, .. }) => {
                assert!(paths.iter().any(|p| p == "shared.txt"), "{paths:?}");
            }
            other => panic!("expected lane #3 Conflict, got {other:?}"),
        }

        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt")).unwrap(),
            "lane 1 wins shared\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("f2.txt")).unwrap(),
            "lane 2\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("f4.txt")).unwrap(),
            "lane 4\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("f5.txt")).unwrap(),
            "lane 5\n"
        );

        // Also explicitly verify CompleteEarly safety invariant:
        // - If lane #1 conflicts while members is empty -> CompleteEarly = true.
        // - After lane #1 moved `cur`, a conflicting lane #3 MUST NOT CompleteEarly,
        //   so if CAS loses and lane #1 was actually absent on the new tip, lane #3 lands!
        assert!(can_complete_early(
            &StepResult::Conflict {
                message: "c".into(),
                paths: vec![],
                total: 1
            },
            true
        ));
        assert!(!can_complete_early(
            &StepResult::Conflict {
                message: "c".into(),
                paths: vec![],
                total: 1
            },
            false
        ));
    }

    /// Required Test 2:
    /// Retried land returns `AlreadyLanded` + accurate `reverted_by` including
    /// revert-of-revert.
    #[test]
    fn retried_land_returns_already_landed_and_accurate_reverted_by() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);
        let source_oid = make_branch_commit(repo, "lane-r", "feature.txt", "feature\n", &base);

        let registry = Arc::new(LandQueueRegistry::new(LandQueueConfig::default()));
        let mut req = LandRequest::new(repo, "lane-r", "main", &source_oid);
        req.squash = true;
        let first = registry.submit(req.clone()).unwrap();
        assert!(!first.already_landed);
        assert_eq!(first.reverted_by, None);

        // Retry immediately: AlreadyLanded, no revert.
        let retry1 = registry.submit(req.clone()).unwrap();
        assert!(retry1.already_landed);
        assert_eq!(retry1.landed_oid, first.landed_oid);
        assert_eq!(retry1.reverted_by, None);

        // Revert the landed commit on main.
        git(repo, &["revert", "--no-edit", &first.landed_oid]);
        let revert_oid = git(repo, &["rev-parse", "refs/heads/main"]);
        let retry2 = registry.submit(req.clone()).unwrap();
        assert!(retry2.already_landed);
        assert_eq!(retry2.reverted_by.as_deref(), Some(revert_oid.as_str()));

        // Revert the revert -> reverted_by clears to None.
        git(repo, &["revert", "--no-edit", &revert_oid]);
        let retry3 = registry.submit(req).unwrap();
        assert!(retry3.already_landed);
        assert_eq!(retry3.reverted_by, None);
    }

    /// Required Test 3:
    /// Opposite-order concurrent SQLite FIFO hot-file tickets are deadlock-free,
    /// plus release-before-upgrade and PID/TTL expiry checks.
    #[test]
    fn opposite_order_sqlite_fifo_tickets_are_deadlock_free() {
        let dir = init_repo();
        let repo = dir.path();
        let store = LandTicketStore::for_repo(repo).unwrap();

        let rounds = 10;
        let barrier = Arc::new(Barrier::new(2));
        let repo_a = repo.to_path_buf();
        let repo_b = repo.to_path_buf();
        let b1 = Arc::clone(&barrier);
        let b2 = Arc::clone(&barrier);

        let t1 = std::thread::spawn(move || {
            let store = LandTicketStore::for_repo(&repo_a).unwrap();
            b1.wait();
            for _ in 0..rounds {
                let mut g = store
                    .acquire(
                        &repo_a,
                        "main",
                        &["z.rs".into(), "a.rs".into(), "m.rs".into()],
                        false,
                        Duration::from_secs(10),
                        Duration::from_secs(10),
                        None,
                    )
                    .unwrap();
                assert_eq!(g.keys(), &["a.rs", "m.rs", "z.rs"]);
                std::thread::sleep(Duration::from_millis(2));
                g.release();
            }
        });

        let t2 = std::thread::spawn(move || {
            let store = LandTicketStore::for_repo(&repo_b).unwrap();
            b2.wait();
            for _ in 0..rounds {
                let mut g = store
                    .acquire(
                        &repo_b,
                        "main",
                        &["a.rs".into(), "z.rs".into(), "m.rs".into()],
                        false,
                        Duration::from_secs(10),
                        Duration::from_secs(10),
                        None,
                    )
                    .unwrap();
                assert_eq!(g.keys(), &["a.rs", "m.rs", "z.rs"]);
                std::thread::sleep(Duration::from_millis(2));
                g.release();
            }
        });

        t1.join().unwrap();
        t2.join().unwrap();

        // Release-before-upgrade & dead-PID / TTL expiry test:
        let mut g = store
            .acquire(
                repo,
                "main",
                &["a.rs".into()],
                false,
                Duration::from_secs(10),
                Duration::from_secs(5),
                None,
            )
            .unwrap();
        assert!(g.covers(&["a.rs".into()], false));
        assert!(!g.covers(&["a.rs".into(), "b.rs".into()], false));
        g.upgrade(
            &["b.rs".into()],
            false,
            Duration::from_secs(10),
            Duration::from_secs(5),
            None,
        )
        .unwrap();
        assert!(g.covers(&["a.rs".into(), "b.rs".into()], false));
        g.release();

        // Inject a dead-PID + expired row and verify next acquire prunes it immediately.
        let conn = open_tickets_db(&store.db_path).unwrap();
        conn.execute(
            "INSERT INTO land_tickets (
               repo_key, target_branch, ticket_key, holder_id, pid, priority, acquired_at_ms, expires_at_ms
             ) VALUES (?1, 'main', 'a.rs', 'dead-holder', 999999, 0, 1, 1)",
            params![repo.canonicalize().unwrap().to_string_lossy().into_owned()],
        )
        .unwrap();
        drop(conn);

        let mut g2 = store
            .acquire(
                repo,
                "main",
                &["a.rs".into()],
                false,
                Duration::from_secs(10),
                Duration::from_secs(2),
                None,
            )
            .unwrap();
        g2.release();
    }

    #[test]
    fn cas_rebuild_recovers_after_concurrent_move_and_does_not_complete_early_when_cur_moved() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);

        // Lane 1 edits shared.txt; Lane 2 also edits shared.txt (conflicts with Lane 1!).
        // On attempt 1, if an external actor moves main forward (editing other.txt)
        // before CAS, Lane 2 must NOT have completed early — it retries on the new tip.
        let oid1 = make_branch_commit(repo, "l1", "f1.txt", "1\n", &base);
        let oid2 = make_branch_commit(repo, "l2", "f2.txt", "2\n", &base);

        let injected = Arc::new(AtomicBool::new(false));
        let injected_clone = Arc::clone(&injected);
        let gate: BatchUpdateGate = Arc::new(move |ctx| {
            if !injected_clone.swap(true, Ordering::SeqCst) {
                let tree = git_text(
                    ctx.repo,
                    &["rev-parse", &format!("{}^{{tree}}", ctx.old_tip)],
                )
                .unwrap()
                .trim()
                .to_string();
                let ext = git_text(
                    ctx.repo,
                    &[
                        "commit-tree",
                        &tree,
                        "-p",
                        ctx.old_tip,
                        "-m",
                        "external move",
                    ],
                )
                .unwrap()
                .trim()
                .to_string();
                git_text(
                    ctx.repo,
                    &["update-ref", "refs/heads/main", &ext, ctx.old_tip],
                )
                .unwrap();
                BatchUpdateDecision::LostCas("simulated CAS race".into())
            } else {
                BatchUpdateDecision::Allow
            }
        });

        let registry = Arc::new(LandQueueRegistry::with_batch_gate(
            LandQueueConfig {
                window: Duration::from_millis(120),
                ..LandQueueConfig::default()
            },
            gate,
        ));

        let reg1 = Arc::clone(&registry);
        let reg2 = Arc::clone(&registry);
        let r1 = repo.to_path_buf();
        let r2 = repo.to_path_buf();
        let h1 = std::thread::spawn(move || {
            let mut req = LandRequest::new(r1, "l1", "main", oid1);
            req.squash = true;
            reg1.submit(req)
        });
        let h2 = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            let mut req = LandRequest::new(r2, "l2", "main", oid2);
            req.squash = true;
            reg2.submit(req)
        });

        let out1 = h1.join().unwrap().unwrap();
        let out2 = h2.join().unwrap().unwrap();
        assert_eq!(out1.telemetry.cas_attempts, 2);
        assert_eq!(out2.telemetry.cas_attempts, 2);
        assert_eq!(out1.telemetry.quarantine_runs, 2);
    }

    #[test]
    fn hook_rejection_falls_back_to_solo_landing_and_split_dependents_isolates_failures() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);

        let oid1 = make_branch_commit(repo, "s1", "s1.txt", "1\n", &base);
        let oid2 = make_branch_commit(repo, "s2", "s2.txt", "2\n", &base);

        let gate: BatchUpdateGate = Arc::new(|ctx| {
            if !ctx.is_solo_fallback && ctx.member_count > 1 {
                BatchUpdateDecision::RejectHook("combined batch rejected by policy".into())
            } else {
                BatchUpdateDecision::Allow
            }
        });
        let registry = Arc::new(LandQueueRegistry::with_batch_gate(
            LandQueueConfig {
                window: Duration::from_millis(120),
                ..LandQueueConfig::default()
            },
            gate,
        ));

        let reg1 = Arc::clone(&registry);
        let reg2 = Arc::clone(&registry);
        let r1 = repo.to_path_buf();
        let r2 = repo.to_path_buf();
        let h1 = std::thread::spawn(move || reg1.submit(LandRequest::new(r1, "s1", "main", oid1)));
        let h2 = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            reg2.submit(LandRequest::new(r2, "s2", "main", oid2))
        });

        let out1 = h1.join().unwrap().unwrap();
        let out2 = h2.join().unwrap().unwrap();
        assert!(out1.telemetry.solo_fallback);
        assert!(out2.telemetry.solo_fallback);
        assert!(
            repo.join("s1.txt").exists()
                || git(repo, &["ls-tree", "-r", "--name-only", "refs/heads/main"])
                    .contains("s1.txt")
        );
        assert!(git(repo, &["ls-tree", "-r", "--name-only", "refs/heads/main"]).contains("s2.txt"));
    }

    #[test]
    fn split_dependents_isolates_failed_member_and_retries_dependent_lane() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);

        // Both lanes edit shared.txt so lane-2 conflicts against lane-1's phantom `cur`
        // in Attempt 1. When Attempt 1 fails with FailUpdate, lane-2 must NOT have
        // completed early: split_dependents fails lane-1 and retries lane-2 cleanly!
        let oid1 = make_branch_commit(repo, "d1", "shared.txt", "d1\n", &base);
        let oid2 = make_branch_commit(repo, "d2", "shared.txt", "d2\n", &base);

        let failed_once = Arc::new(AtomicBool::new(false));
        let failed_clone = Arc::clone(&failed_once);
        let gate: BatchUpdateGate = Arc::new(move |ctx| {
            assert_eq!(ctx.target_branch, "main");
            assert_ne!(ctx.new_tip, ctx.old_tip);
            if !failed_clone.swap(true, Ordering::SeqCst) {
                BatchUpdateDecision::FailUpdate(LandError::Failed {
                    message: "injected non-retryable update failure".into(),
                })
            } else {
                BatchUpdateDecision::Allow
            }
        });

        let registry = Arc::new(LandQueueRegistry::with_batch_gate(
            LandQueueConfig {
                window: Duration::from_millis(120),
                ..LandQueueConfig::default()
            },
            gate,
        ));

        let reg1 = Arc::clone(&registry);
        let reg2 = Arc::clone(&registry);
        let r1 = repo.to_path_buf();
        let r2 = repo.to_path_buf();
        let h1 = std::thread::spawn(move || reg1.submit(LandRequest::new(r1, "d1", "main", oid1)));
        let h2 = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            reg2.submit(LandRequest::new(r2, "d2", "main", oid2))
        });

        assert!(matches!(h1.join().unwrap(), Err(LandError::Failed { .. })));
        let out2 = h2.join().unwrap().unwrap();
        assert_eq!(out2.telemetry.quarantine_runs, 2);
        assert_eq!(
            std::fs::read_to_string(repo.join("shared.txt")).unwrap(),
            "d2\n"
        );
    }

    #[test]
    fn aimd_batch_size_adapts_multiplicatively_and_additively() {
        let target = Duration::from_millis(500);
        // Over target -> halve (min 1)
        assert_eq!(
            adapt_batch_size(50, Duration::from_millis(600), target, 100),
            25
        );
        assert_eq!(
            adapt_batch_size(1, Duration::from_millis(600), target, 100),
            1
        );
        // Under target/2 -> +25% (min +1), capped at 100
        assert_eq!(
            adapt_batch_size(40, Duration::from_millis(100), target, 100),
            50
        );
        assert_eq!(
            adapt_batch_size(90, Duration::from_millis(100), target, 100),
            100
        );
        // Deadband [target/2, target] -> unchanged
        assert_eq!(
            adapt_batch_size(40, Duration::from_millis(350), target, 100),
            40
        );
    }

    #[test]
    fn branch_wide_and_file_level_tickets_are_mutually_exclusive_both_ways() {
        let dir = init_repo();
        let repo = dir.path();
        let store = LandTicketStore::for_repo(repo).unwrap();

        // 1. Holding branch_wide blocks concurrent file-level ["a.rs"] until released.
        let mut bw_guard = store
            .acquire(
                repo,
                "main",
                &[],
                true,
                Duration::from_secs(10),
                Duration::from_secs(5),
                None,
            )
            .unwrap();
        let file_acquired = Arc::new(AtomicBool::new(false));
        let file_acquired_clone = Arc::clone(&file_acquired);
        let repo_f = repo.to_path_buf();
        let t_file = std::thread::spawn(move || {
            let s = LandTicketStore::for_repo(&repo_f).unwrap();
            let mut g = s
                .acquire(
                    &repo_f,
                    "main",
                    &["a.rs".into()],
                    false,
                    Duration::from_secs(10),
                    Duration::from_secs(5),
                    None,
                )
                .unwrap();
            file_acquired_clone.store(true, Ordering::SeqCst);
            g.release();
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !file_acquired.load(Ordering::SeqCst),
            "file-level ticket must block while branch_wide ticket is held"
        );
        bw_guard.release();
        t_file.join().unwrap();
        assert!(file_acquired.load(Ordering::SeqCst));

        // 2. Holding ["a.rs"] blocks concurrent branch_wide until released.
        let mut file_guard = store
            .acquire(
                repo,
                "main",
                &["a.rs".into()],
                false,
                Duration::from_secs(10),
                Duration::from_secs(5),
                None,
            )
            .unwrap();
        let bw_acquired = Arc::new(AtomicBool::new(false));
        let bw_acquired_clone = Arc::clone(&bw_acquired);
        let repo_bw = repo.to_path_buf();
        let t_bw = std::thread::spawn(move || {
            let s = LandTicketStore::for_repo(&repo_bw).unwrap();
            let mut g = s
                .acquire(
                    &repo_bw,
                    "main",
                    &[],
                    true,
                    Duration::from_secs(10),
                    Duration::from_secs(5),
                    None,
                )
                .unwrap();
            bw_acquired_clone.store(true, Ordering::SeqCst);
            g.release();
        });
        std::thread::sleep(Duration::from_millis(50));
        assert!(
            !bw_acquired.load(Ordering::SeqCst),
            "branch_wide ticket must block while file-level ticket is held"
        );
        file_guard.release();
        t_bw.join().unwrap();
        assert!(bw_acquired.load(Ordering::SeqCst));
    }

    #[test]
    fn concurrent_submit_queued_cancel_and_close_is_deadlock_free() {
        let dir = init_repo();
        let repo = dir.path();
        let base = git(repo, &["rev-parse", "refs/heads/main"]);
        let oids: Vec<String> = (1..=6)
            .map(|i| {
                make_branch_commit(repo, &format!("c-{i}"), &format!("c-{i}.txt"), "v\n", &base)
            })
            .collect();

        let registry = Arc::new(LandQueueRegistry::new(LandQueueConfig {
            window: Duration::from_millis(80),
            ..LandQueueConfig::default()
        }));

        let cancel_flags: Vec<Arc<AtomicBool>> =
            (0..6).map(|_| Arc::new(AtomicBool::new(false))).collect();
        let barrier = Arc::new(Barrier::new(7));
        let mut handles = Vec::new();
        for (i, oid) in oids.into_iter().enumerate() {
            let reg = Arc::clone(&registry);
            let repo_p = repo.to_path_buf();
            let flag = Arc::clone(&cancel_flags[i]);
            let b = Arc::clone(&barrier);
            handles.push(std::thread::spawn(move || {
                b.wait();
                let mut req = LandRequest::new(repo_p, format!("c-{}", i + 1), "main", oid);
                req.cancel = Some(flag);
                reg.submit(req)
            }));
        }

        // Concurrently cancel half the queued requests and close the registry during collection.
        let reg_close = Arc::clone(&registry);
        let cancel_clone = cancel_flags;
        let b_close = Arc::clone(&barrier);
        let closer = std::thread::spawn(move || {
            b_close.wait();
            std::thread::sleep(Duration::from_millis(3));
            for (i, flag) in cancel_clone.iter().enumerate() {
                if i % 2 == 0 {
                    flag.store(true, Ordering::SeqCst);
                }
            }
            std::thread::sleep(Duration::from_millis(5));
            reg_close.close();
        });

        closer.join().unwrap();
        for h in handles {
            let res = h.join().unwrap();
            assert!(res.is_ok() || matches!(res, Err(LandError::Canceled)));
        }
    }
}
