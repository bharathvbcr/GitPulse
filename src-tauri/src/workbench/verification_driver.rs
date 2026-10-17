//! Starts verification for an agent's held `done`, and settles the card when
//! the verdict comes back.
//!
//! An agent's `gitpulse_complete_task(done)` lands in `review` unless Manvi
//! vouched for the work (`verification_gate`). The MCP server that took the
//! claim is a separate process with no sidecar, so it leaves the evidence of
//! the claim in the card's logs (a held block naming the worktree and head it
//! was made at) and this driver, which lives in the app that owns the Manvi
//! sidecar, acts on it from a background thread. Nothing here runs on the
//! agent's MCP response path.
//!
//! The rules, each pinned by a test:
//! - A claim is acted on only while the worktree is still at the head it was
//!   made at. A newer head is a newer claim and has its own held block.
//! - One verification per (task, head): the request id is derived from both,
//!   so Manvi replays rather than starts a second, and this process keeps an
//!   attempt table so it does not even ask twice. At most [`MAX_ACTIVE`]
//!   attempts are in flight; the rest wait for the next sweep.
//! - `passed` moves the card to `done` only if it is still in `review` and
//!   still at the revision the driver last saw. A person's move or edit in
//!   between wins, and the verdict is recorded without moving anything.
//! - `failed` and `degraded` keep the card in review with the verdict
//!   recorded. `degraded` is never a pass.
//! - A sidecar that is down, a start Manvi refuses, or a verdict that never
//!   arrives is recorded as `could_not_run`: its own state, in the logs and
//!   as an evidence row, so it reads as neither a failure nor a pass. The gate
//!   treats it as a hold. It is not retried on its own; Verify on the card is.
//! - A head or base that could not be read because git was deferred under
//!   load, or was waiting for a process slot, writes nothing and is asked
//!   again on the next sweep. A `could_not_run` whose cause is that deferral
//!   is asked again once the head can be read. A sidecar that is down stays
//!   terminal.
//! - A host that is shutting down stops the sweep without writing.

use super::intake::{self, HELD_MARKER};
use super::verification_gate::{self as gate, Gate, Record, EVIDENCE_TYPE, STATE_COULD_NOT_RUN};
use super::{query, WorkbenchError, WorkbenchState};
use dc_store::Store;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::{Duration, Instant};

/// Verifications this driver keeps in flight at once. Each is a critic turn
/// and a verification command in Manvi, so the bound is on cost.
pub(crate) const MAX_ACTIVE: usize = 2;
/// How long a started verification may stay unsettled before it is recorded
/// as one that could not run. Manvi bounds a job at 15 minutes itself.
const WAIT_LIMIT: Duration = Duration::from_secs(20 * 60);
const SWEEP_PERIOD: Duration = Duration::from_secs(3);
const TARGET_PREFIX: &str = "verification-target: ";
/// Review cards read per sweep, in pages of 200.
const MAX_PAGES: usize = 5;

/// The work an agent's claim named: where, and at which commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub worktree: String,
    pub head: String,
}

/// The line the held block carries for [`target_of`]. Empty when the head
/// could not be read: a claim that cannot be tied to a commit is not started.
pub(crate) fn target_line(repo_path: &str) -> String {
    let Ok(local) = intake::resolve(repo_path) else {
        return String::new();
    };
    match gate::current_head(&local.path) {
        Ok(head) => format!(
            "\n{TARGET_PREFIX}{}",
            json!({"worktree": local.path, "head": head})
        ),
        Err(_) => String::new(),
    }
}

/// The target of a card's held claim: a card in `review` whose last held block
/// names one.
pub(crate) fn target_of(item: &Value) -> Option<Target> {
    if item["status"] != "review" {
        return None;
    }
    let logs = item["logs"].as_str()?;
    let (_, body) = intake::last_block(logs, HELD_MARKER)?;
    let line = body.lines().find_map(|l| l.strip_prefix(TARGET_PREFIX))?;
    let parsed: Value = serde_json::from_str(line).ok()?;
    let worktree = parsed["worktree"].as_str()?.trim();
    let head = parsed["head"].as_str()?.trim();
    (!worktree.is_empty() && !head.is_empty()).then(|| Target {
        worktree: worktree.into(),
        head: head.into(),
    })
}

/// Everything the driver reads from outside the store: the sidecar, git, the
/// clock and whether the host is going away. Tests supply their own.
pub(crate) trait Backend {
    /// Manvi's `work.verify.task` with validated params.
    fn start(&self, params: Value) -> Result<Value, WorkbenchError>;
    fn head_of(&self, worktree: &str) -> Result<String, String>;
    /// The revision verification diffs against: where `head` left the default
    /// branch, or its first parent when `head` is on that branch.
    fn base_of(&self, worktree: &str, head: &str) -> Result<String, String>;
    fn now(&self) -> Instant;
    fn cancelled(&self) -> bool;
}

/// A verification this process started or adopted.
#[derive(Debug, Clone)]
struct Attempt {
    started: Instant,
    /// The card's revision when the attempt was first seen; a card that moved
    /// on from it is a card a person touched.
    baseline: i64,
}

/// What one sweep did to one card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Skip,
    Wait,
    Start,
    Promote,
    /// A settled verdict that is not a pass: record it, move nothing.
    Park,
    /// A verification that did not run, with why.
    CouldNotRun(String),
    /// A pass for a card a person touched meanwhile: record it, move nothing.
    PassedButChanged,
}

/// What the driver knows about one held card.
pub(crate) struct Facts<'a> {
    pub revision: i64,
    pub logs: &'a str,
    pub target: &'a Target,
    pub current_head: Result<String, String>,
    pub latest: Result<Option<Record>, String>,
    pub attempt: Option<(Duration, i64)>,
    pub item_id: &'a str,
}

/// The state machine. Pure: every input is in `facts`.
pub(crate) fn decide(facts: &Facts<'_>) -> Step {
    let head = match &facts.current_head {
        Ok(head) => head,
        // The spawn was declined. Nothing was read, so this is not a verdict
        // and the next sweep asks again. Writing `could_not_run` here made a
        // two-second rate-limit into a permanent hold for that commit.
        Err(error) if git_question_unasked(error) => return Step::Skip,
        Err(error) => {
            return Step::CouldNotRun(format!("the worktree's head could not be read: {error}"))
        }
    };
    if *head != facts.target.head {
        return Step::Skip;
    }
    let record = match &facts.latest {
        // A store that cannot be read now is read again on the next sweep.
        Err(_) => return Step::Skip,
        Ok(record) => record
            .as_ref()
            .filter(|r| r.head == facts.target.head && r.item_id == facts.item_id),
    };
    let waited = |elapsed: Duration| {
        if elapsed >= WAIT_LIMIT {
            Step::CouldNotRun(format!(
                "no verdict arrived within {} minutes",
                WAIT_LIMIT.as_secs() / 60
            ))
        } else {
            Step::Wait
        }
    };
    match record {
        None => match facts.attempt {
            Some((elapsed, _)) => waited(elapsed),
            None => Step::Start,
        },
        Some(r) if r.state == STATE_COULD_NOT_RUN => {
            // A deferral stored by an older build is the same unasked
            // question. A sidecar that was down, or any other cause, stays
            // terminal: Verify on the card is what retries those.
            if r.degraded.iter().any(|line| git_question_unasked(line)) {
                match facts.attempt {
                    Some((elapsed, _)) => waited(elapsed),
                    None => Step::Start,
                }
            } else {
                Step::Skip
            }
        }
        Some(r) if r.state != "settled" => {
            waited(facts.attempt.map_or(Duration::ZERO, |(elapsed, _)| elapsed))
        }
        // A verdict already written into the card's history has been acted on
        // (or deliberately not): a card a person left in review after a pass
        // is not moved by a later sweep.
        Some(r) if facts.logs.contains(&recorded_marker(&r.request_id)) => Step::Skip,
        Some(r) if r.outcome == "passed" => match facts.attempt {
            Some((_, baseline)) if baseline != facts.revision => Step::PassedButChanged,
            _ => Step::Promote,
        },
        Some(_) => Step::Park,
    }
}

/// The driver's memory between sweeps.
#[derive(Default)]
pub(crate) struct Driver {
    attempts: HashMap<(String, String), Attempt>,
}

/// What a sweep did, for the log line and for tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Report {
    pub started: usize,
    pub promoted: usize,
    pub parked: usize,
    pub could_not_run: usize,
}

/// The line a verdict note carries so a later sweep knows it was acted on.
fn recorded_marker(request: &str) -> String {
    format!("verdict recorded for request {request}")
}

fn request_id(item_id: &str, head: &str) -> String {
    format!("auto-{item_id}-{head}")
}

fn held_cards(store: &Store) -> Result<Vec<Value>, WorkbenchError> {
    let mut cards = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let mut input = json!({"status": "review", "archived": false, "limit": 200});
        if let Some(cursor) = &cursor {
            input["cursor"] = json!(cursor);
        }
        let page = query(store, "items.list", &input.to_string())?;
        let items = page["items"]
            .as_array()
            .ok_or_else(|| WorkbenchError::new("protocol_error", "Invalid task page."))?;
        for card in items {
            let id = card["id"].as_str().unwrap_or_default();
            // The list view omits logs; the claim lives there.
            if let Some(full) = intake::get_item(store, id)? {
                cards.push(full);
            }
        }
        match page["next_cursor"].as_str() {
            Some(next) if page["has_more"] == true => cursor = Some(next.into()),
            _ => break,
        }
    }
    Ok(cards)
}

fn repository_identity(store: &Store, item: &Value) -> Result<String, WorkbenchError> {
    let id = item["primary_repository_id"].as_str().unwrap_or_default();
    let repository = query(store, "repositories.get", &json!({"id": id}).to_string())?;
    Ok(repository["item"]["identity_key"]
        .as_str()
        .unwrap_or_default()
        .to_owned())
}

/// The evidence row a verification that could not run leaves, in the shape of
/// Manvi's record so the same reader sees it as the newest verdict.
fn could_not_run_record(item_id: &str, request: &str, target: &Target, why: &str) -> Value {
    json!({
        "request_id": request, "item_id": item_id, "state": STATE_COULD_NOT_RUN,
        "outcome": "", "worktree": target.worktree, "head": target.head,
        "criteria": [], "degraded": [why],
    })
}

impl Driver {
    /// One pass over the held cards. Never panics on a bad card: each card's
    /// failure is that card's only.
    pub(crate) fn sweep(&mut self, store: &Store, backend: &dyn Backend) -> Report {
        let mut report = Report::default();
        let cards = match held_cards(store) {
            Ok(cards) => cards,
            Err(error) => {
                log::warn!(target: "workbench", "verification driver could not list held cards: {}", error.message);
                return report;
            }
        };
        let mut seen = HashSet::new();
        for card in &cards {
            if backend.cancelled() {
                return report;
            }
            let Some(target) = target_of(card) else {
                continue;
            };
            let id = card["id"].as_str().unwrap_or_default().to_owned();
            let key = (id.clone(), target.head.clone());
            seen.insert(key.clone());
            self.settle(store, backend, card, &id, &target, &key, &mut report);
        }
        // A card that was moved, edited out of its claim, deleted or left for
        // a newer head is no longer waited on.
        self.attempts.retain(|key, _| seen.contains(key));
        report
    }

    #[allow(clippy::too_many_arguments)]
    fn settle(
        &mut self,
        store: &Store,
        backend: &dyn Backend,
        card: &Value,
        id: &str,
        target: &Target,
        key: &(String, String),
        report: &mut Report,
    ) {
        let revision = card["revision"].as_i64().unwrap_or_default();
        let logs = card["logs"].as_str().unwrap_or_default();
        let now = backend.now();
        let step = decide(&Facts {
            revision,
            logs,
            target,
            current_head: backend.head_of(&target.worktree),
            latest: gate::latest(store, id),
            attempt: self
                .attempts
                .get(key)
                .map(|a| (now.saturating_duration_since(a.started), a.baseline)),
            item_id: id,
        });
        match step {
            Step::Skip => {}
            Step::Wait => {
                self.attempts.entry(key.clone()).or_insert(Attempt {
                    started: now,
                    baseline: revision,
                });
            }
            Step::Start => {
                if self.attempts.len() >= MAX_ACTIVE {
                    return;
                }
                match start(store, backend, card, id, target) {
                    Ok(()) => {
                        self.attempts.insert(
                            key.clone(),
                            Attempt {
                                started: now,
                                baseline: revision,
                            },
                        );
                        report.started += 1;
                    }
                    // Same class as a deferred head read: the base was not
                    // read, so there is no verdict to store.
                    Err(why) if git_question_unasked(&why) => {}
                    Err(why) => {
                        self.could_not_run(store, card, id, target, &why, report);
                    }
                }
            }
            Step::Promote => {
                let outcome = gate::check(
                    store,
                    id,
                    &repository_identity(store, card).unwrap_or_default(),
                );
                match outcome {
                    Gate::Passed(verdict) => {
                        let block = format!(
                            "--- Verification passed; moved to done ({}) ---\n{verdict}",
                            intake::timestamp(intake::now_millis())
                        );
                        if intake::annotate_task(store, id, revision, Some("done"), &block).is_ok()
                        {
                            report.promoted += 1;
                        }
                        // A conflict means a person got there first; the next
                        // sweep re-reads the card and decides again.
                    }
                    Gate::Held(reason) => {
                        log::warn!(target: "workbench", "verification of {id} passed but the gate holds it: {reason}");
                    }
                }
                self.attempts.remove(key);
            }
            Step::Park | Step::PassedButChanged => {
                let record = gate::latest(store, id).ok().flatten().unwrap_or_default();
                let note = if step == Step::Park {
                    format!(
                        "--- Verification {} ({}) ---\n{}\n{}. The card stays in review.",
                        if record.outcome.is_empty() {
                            "unsettled"
                        } else {
                            &record.outcome
                        },
                        intake::timestamp(intake::now_millis()),
                        gate::summarize(&record),
                        recorded_marker(&record.request_id)
                    )
                } else {
                    format!(
                        "--- Verification passed; the card changed meanwhile, so it was left where it is ({}) ---\n{}\n{}.",
                        intake::timestamp(intake::now_millis()),
                        gate::summarize(&record),
                        recorded_marker(&record.request_id)
                    )
                };
                if intake::annotate_task(store, id, revision, None, &note).is_ok() {
                    report.parked += 1;
                }
                self.attempts.remove(key);
            }
            Step::CouldNotRun(why) => {
                self.could_not_run(store, card, id, target, &why, report);
                self.attempts.remove(key);
            }
        }
    }

    fn could_not_run(
        &mut self,
        store: &Store,
        card: &Value,
        id: &str,
        target: &Target,
        why: &str,
        report: &mut Report,
    ) {
        let record = could_not_run_record(id, &request_id(id, &target.head), target, why);
        // The evidence row first: it is what stops the next sweep trying
        // again. If it cannot be written nothing is claimed.
        if store
            .evidence_append(EVIDENCE_TYPE, Some(id), None, None, &record.to_string())
            .is_err()
        {
            return;
        }
        let block = format!(
            "--- Verification could not run ({}) ---\n{why}\nThis is not a failed verification; use Verify on the card to try again.",
            intake::timestamp(intake::now_millis())
        );
        let revision = card["revision"].as_i64().unwrap_or_default();
        let _ = intake::annotate_task(store, id, revision, None, &block);
        report.could_not_run += 1;
    }
}

/// Ask Manvi to verify `target` for card `id`.
fn start(
    store: &Store,
    backend: &dyn Backend,
    card: &Value,
    id: &str,
    target: &Target,
) -> Result<(), String> {
    let identity = repository_identity(store, card).map_err(|e| e.message)?;
    let local = intake::resolve(&target.worktree).map_err(|e| e.message)?;
    if local.identity != identity {
        return Err("the claimed worktree belongs to another repository".into());
    }
    let base = backend.base_of(&target.worktree, &target.head)?;
    let input = json!({
        "id": id, "request_id": request_id(id, &target.head),
        "worktree": target.worktree, "base": base, "head": target.head,
    });
    let params = gate::start_params(&input.to_string()).map_err(|e| e.message)?;
    backend
        .start(params)
        .map(|_| ())
        .map_err(|e| format!("the verification could not be started: {}", e.message))
}

/// The card's verdict as the board shows it, read from the store alone so it
/// works with the sidecar down.
///
/// `state` is one of `none`, `running`, `passed`, `failed`, `degraded`,
/// `could_not_run`. `stale` says the verdict was reached for a commit the
/// worktree has since moved past.
pub(crate) fn status(
    store: &Store,
    backend: &dyn Backend,
    id: &str,
) -> Result<Value, WorkbenchError> {
    let record = gate::latest(store, id).map_err(|m| WorkbenchError::new("store_error", m))?;
    Ok(describe(record, backend, &mut HashMap::new(), true))
}

/// Most cards one batched read answers for: a board page is at most 30 cards
/// in each of seven columns.
pub(crate) const MAX_BATCH: usize = 256;

/// The verdict of many cards in one read, for the board. Only the cards that
/// have ever been verified are named; the rest are `none` by absence. Any card
/// whose record cannot be read fails the whole read, so the board says so
/// rather than drawing the others as if the failure were a clean "none".
pub(crate) fn status_batch(
    store: &Store,
    backend: &dyn Backend,
    ids: &[String],
) -> Result<Value, WorkbenchError> {
    if ids.len() > MAX_BATCH {
        return Err(WorkbenchError::new(
            "invalid_input",
            format!("A verdict read names at most {MAX_BATCH} tasks."),
        ));
    }
    let mut heads = HashMap::new();
    let mut items = serde_json::Map::new();
    for id in ids {
        let record = gate::latest(store, id).map_err(|m| WorkbenchError::new("store_error", m))?;
        if record.is_some() {
            items.insert(id.clone(), describe(record, backend, &mut heads, false));
        }
    }
    Ok(json!({"ok": true, "items": items}))
}

/// One record as the board shows it. `heads` memoizes each worktree's head
/// for the duration of a read; `full` includes the record itself.
fn describe(
    record: Option<Record>,
    backend: &dyn Backend,
    heads: &mut HashMap<String, Result<String, String>>,
    full: bool,
) -> Value {
    let Some(record) = record else {
        return json!({"ok": true, "state": "none", "stale": false, "record": null});
    };
    let state = if record.state == STATE_COULD_NOT_RUN {
        "could_not_run"
    } else if record.state != "settled" {
        "running"
    } else {
        match record.outcome.as_str() {
            "passed" => "passed",
            "failed" => "failed",
            // Anything else Manvi settles on is not a pass.
            _ => "degraded",
        }
    };
    let stale = !record.worktree.is_empty()
        && heads
            .entry(record.worktree.clone())
            .or_insert_with(|| backend.head_of(&record.worktree))
            .as_ref()
            .is_ok_and(|head| *head != record.head);
    if full {
        json!({"ok": true, "state": state, "stale": stale, "record": record})
    } else {
        json!({"ok": true, "state": state, "stale": stale})
    }
}

/// A card's manual Verify: the card's worktree is the one its newest claim
/// named while it still exists, else the repository's checkout — when no claim
/// names a worktree, or when the claimed worktree is gone and the claimed
/// commit is in the checkout's HEAD (merged, then torn down). A worktree that
/// is gone with its commit nowhere in the checkout was abandoned, not merged:
/// judging the checkout then would pass work that never landed, so it is
/// refused. Each press is its own request, so it can re-run a verdict the
/// automatic one already reached.
pub(crate) fn run_now(
    store: &Store,
    backend: &dyn Backend,
    id: &str,
) -> Result<Value, WorkbenchError> {
    let card = intake::get_item(store, id)?
        .ok_or_else(|| WorkbenchError::new("not_found", "The task does not exist."))?;
    let identity = repository_identity(store, &card)?;
    let checkout = || {
        identity
            .strip_prefix("local:")
            .and_then(|git_dir| git_dir.strip_suffix("/.git").or(Some(git_dir)))
            .filter(|path| Path::new(path).is_dir())
            .map(str::to_owned)
            .ok_or_else(|| {
                WorkbenchError::new(
                    "repository_unavailable",
                    "This task's repository has no local checkout to verify.",
                )
            })
    };
    let worktree = match target_of(&card) {
        None => checkout()?,
        Some(target) if Path::new(&target.worktree).is_dir() => target.worktree,
        // The claimed worktree is gone. Its work is judged in the checkout
        // only once the checkout holds the claimed commit; otherwise the
        // checkout would stand in for work that never reached it.
        Some(target) => {
            let path = checkout()?;
            let merged = crate::harness::review::contains(Path::new(&path), "HEAD", &target.head)
                .map_err(|why| {
                    WorkbenchError::new(
                        "repository_unavailable",
                        format!("The claimed worktree is gone and whether its work reached the checkout could not be read: {why}"),
                    )
                })?;
            if !merged {
                return Err(WorkbenchError::new(
                    "repository_unavailable",
                    format!(
                        "The claimed worktree {} is gone and its commit {} is not merged into the checkout, so there is no copy of the work to verify.",
                        target.worktree,
                        target.head.get(..12).unwrap_or(&target.head)
                    ),
                ));
            }
            path
        }
    };
    let local = intake::resolve(&worktree)?;
    if local.identity != identity {
        return Err(WorkbenchError::new(
            "repository_unavailable",
            "The worktree belongs to another repository.",
        ));
    }
    let head = backend
        .head_of(&local.path)
        .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
    let base = backend
        .base_of(&local.path, &head)
        .map_err(|e| WorkbenchError::new("repository_unavailable", e))?;
    let request = format!(
        "manual-{id}-{}-{}",
        head.get(..12).unwrap_or(&head),
        intake::now_millis()
    );
    let input = json!({
        "id": id, "request_id": request, "worktree": local.path, "base": base, "head": head,
    });
    let params = gate::start_params(&input.to_string())?;
    backend.start(params)
}

/// True when `err` is a git question that was not asked: the spawn gate
/// deferred it under load, or the child never got a process slot. The command
/// did not run, so the error is not an answer about the repository.
fn git_question_unasked(err: &str) -> bool {
    crate::engine::git_cli::is_deferred_under_load(err)
        || crate::engine::git_cli::is_slot_wait_timeout(err)
}

const NO_PARENT: &str = "the claimed commit is on the default branch and has no parent, so there is no earlier revision to diff against";

/// What a `rev-parse` of `{head}^` means for the verification base.
///
/// A deferred or slot-wait error is returned unchanged. An unknown parent is
/// the root-commit error. Any other git failure stays that failure.
pub(crate) fn base_from_parent_read(
    read: Result<String, String>,
    head: &str,
) -> Result<String, String> {
    match read {
        Err(err) if git_question_unasked(&err) => Err(err),
        Err(err) if missing_parent(&err) => Err(NO_PARENT.to_owned()),
        Err(err) => Err(err),
        Ok(parent) => {
            let parent = parent.trim().to_owned();
            if parent.is_empty() || parent == head {
                Err(NO_PARENT.to_owned())
            } else {
                Ok(parent)
            }
        }
    }
}

fn missing_parent(err: &str) -> bool {
    err.contains("Needed a single revision")
        || err.contains("unknown revision")
        || err.contains("bad revision")
}

/// The commit verification diffs against.
///
/// The fork is `merge-base` of the default-branch tip and `head`. A commit
/// that left that branch diffs from the fork. A commit that is on the branch
/// would otherwise diff against itself, an empty range, so the base is its
/// first parent and the tip commit is what gets judged. Several commits
/// landed directly on the default branch still contribute only the tip: the
/// range is one commit. An empty diff is never treated as a pass; this
/// function only refuses to hand back `head` itself. A root commit has no
/// earlier revision and is an error.
///
/// A git question that was not asked is returned as that error. It is not
/// rewritten into "no parent" or "no default branch".
pub(crate) fn verification_base(worktree: &Path, head: &str) -> Result<String, String> {
    let head = resolve_commit(worktree, head)?;
    let fork = default_fork(worktree, &head)?;
    if fork != head {
        return Ok(fork);
    }
    let spec = format!("{head}^");
    base_from_parent_read(
        crate::engine::git_cli::git_text(
            worktree,
            &["rev-parse", "--verify", "--end-of-options", &spec],
        ),
        &head,
    )
}

fn resolve_commit(worktree: &Path, head: &str) -> Result<String, String> {
    let spec = format!("{head}^{{commit}}");
    let full = crate::engine::git_cli::git_text(
        worktree,
        &["rev-parse", "--verify", "--end-of-options", &spec],
    )?;
    let full = full.trim().to_owned();
    if full.is_empty() {
        return Err(format!("the claimed commit {head} could not be resolved"));
    }
    Ok(full)
}

fn default_fork(worktree: &Path, head: &str) -> Result<String, String> {
    let base = crate::engine::git_reader::default_base(worktree)
        .checked()?
        .base
        .map(|(_, oid)| oid)
        .ok_or_else(|| "the repository has no default branch to verify against".to_owned())?;
    let fork = crate::engine::git_cli::git_text(worktree, &["merge-base", "--", &base, head])?;
    let fork = fork.trim().to_owned();
    if fork.is_empty() {
        return Err("merge-base returned no commit".into());
    }
    Ok(fork)
}

/// The live backend: Manvi through the host's profile worker, git through the
/// engine.
pub(crate) struct HostBackend(pub WorkbenchState);

impl Backend for HostBackend {
    fn start(&self, params: Value) -> Result<Value, WorkbenchError> {
        self.0.worker_call_current("work.verify.task", params)
    }
    fn head_of(&self, worktree: &str) -> Result<String, String> {
        gate::current_head(worktree)
    }
    fn base_of(&self, worktree: &str, head: &str) -> Result<String, String> {
        verification_base(Path::new(worktree), head)
    }
    fn now(&self) -> Instant {
        Instant::now()
    }
    fn cancelled(&self) -> bool {
        self.0.check_open().is_err()
    }
}

/// Start the driver thread. A thread that fails to start costs automatic
/// verification only: the Verify action still works.
pub(crate) fn install(host: WorkbenchState) {
    if let Err(error) = crate::supervisor::spawn_service("workbench-verification", move || {
        let backend = HostBackend(host.clone());
        let mut driver = Driver::default();
        // Its own connection: a sweep waits on the sidecar, and the
        // board's reads must not queue behind that.
        let mut store: Option<Store> = None;
        while host.check_open().is_ok() {
            if store.is_none() {
                store = host
                    .profile_path()
                    .ok()
                    .filter(|path| path.exists())
                    .and_then(|path| Store::open(&path).ok());
            }
            if let Some(open) = &store {
                let report = driver.sweep(open, &backend);
                if report != Report::default() {
                    log::info!(target: "workbench", "verification driver: {report:?}");
                }
            }
            std::thread::sleep(SWEEP_PERIOD);
        }
    }) {
        log::warn!(target: "workbench", "verification driver did not start: {error}");
    }
}
