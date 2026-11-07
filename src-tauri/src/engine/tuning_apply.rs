//! The user-clicked apply of recommended git tuning (owner decision,
//! 2026-11-06): the only code that runs a config-writing or maintenance argv.
//!
//! It mirrors the graph writer it sits beside (`GitWriter::commit_graph_write`):
//! the person sees the exact argv, scope and undo of each change first
//! ([`tuning_plan`]), confirms it, and the command layer judges every argv with
//! the command gate before this spawns anything. The rules it adds on top:
//!
//! - one apply per common directory at a time (a second click joins it);
//! - the whole run is bounded by [`TUNING_APPLY_TIMEOUT_SECS`], each step gets
//!   what is left of it, and the person can stop it between and during steps;
//! - an untrusted repository is refused before anything is read or run;
//! - the confirmed argv must be exactly what a fresh plan would run, and only
//!   the argv shapes in `repo_tuning::is_apply_argv` can ever be spawned;
//! - every step that ran leaves a ledger row naming its argv and how it ended.
//!
//! Nothing here is reachable from MCP, a hook, ingest or a timer: the one
//! caller is `cmd_tuning_apply`, which only the Storage panel's confirm click
//! invokes (`tests/tuning_apply_reach.rs` pins that over the source).

use crate::engine::git_cli::{self, ProcessObserver};
use crate::engine::git_writer::record_clicked_write;
use crate::ipc_error::CommandError;
use crate::singleflight::SingleFlight;
use crate::storage::repo_tuning::{
    config_true, is_apply_argv, plan_apply, GitEnv, RepoTuningReport, TuningChange, TuningOutcome,
    TuningPlan, TuningStepResult, TuningStepStatus, TUNING_APPLY_TIMEOUT_SECS,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Git's own output for a config write or `maintenance start` is a line or two.
const OUTPUT_CAP: usize = 64 * 1024;
/// How long an unanswered Stop waits for its apply to register.
const STOP_INTENT_LIFETIME: Duration = Duration::from_secs(5);

type Flight = Result<TuningOutcome, CommandError>;

fn flights() -> &'static SingleFlight<PathBuf, Flight> {
    static FLIGHTS: OnceLock<SingleFlight<PathBuf, Flight>> = OnceLock::new();
    FLIGHTS.get_or_init(Default::default)
}

fn cancels() -> &'static Mutex<HashMap<PathBuf, Arc<AtomicBool>>> {
    static CANCELS: OnceLock<Mutex<HashMap<PathBuf, Arc<AtomicBool>>>> = OnceLock::new();
    CANCELS.get_or_init(Default::default)
}

fn stop_intents() -> &'static Mutex<HashMap<PathBuf, Instant>> {
    static INTENTS: OnceLock<Mutex<HashMap<PathBuf, Instant>>> = OnceLock::new();
    INTENTS.get_or_init(Default::default)
}

/// The change set of the apply running (or about to run) in each common dir,
/// with how many callers hold it. A second click may join an apply only when it
/// asks for the same changes: a different set would be joined and silently
/// dropped, and the person would believe it had been applied.
type RunningSets = Mutex<HashMap<PathBuf, (Vec<TuningChange>, usize)>>;

fn running_sets() -> &'static RunningSets {
    static SETS: OnceLock<RunningSets> = OnceLock::new();
    SETS.get_or_init(Default::default)
}

/// Holds a claim on `common`'s running set until dropped.
struct SetClaim(PathBuf);

impl SetClaim {
    fn claim(common: &Path, requested: &[TuningChange]) -> Result<Self, CommandError> {
        let mut set = requested.to_vec();
        set.sort();
        set.dedup();
        let mut sets = locked(running_sets());
        match sets.get_mut(common) {
            Some((running, holders)) if *running == set => *holders += 1,
            Some(_) => {
                return Err(
                    "A tuning apply with different changes is already running in this \
                     repository; stop it from the Storage panel or wait for it to finish"
                        .to_string()
                        .into(),
                )
            }
            None => {
                sets.insert(common.to_path_buf(), (set, 1));
            }
        }
        Ok(Self(common.to_path_buf()))
    }
}

impl Drop for SetClaim {
    fn drop(&mut self) {
        let mut sets = locked(running_sets());
        if let Some((_, holders)) = sets.get_mut(&self.0) {
            *holders -= 1;
            if *holders == 0 {
                sets.remove(&self.0);
            }
        }
    }
}

fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

struct Cancel(Arc<AtomicBool>);

impl ProcessObserver for Cancel {
    fn cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

/// Registers an apply's cancel flag (already set when a stop was waiting for
/// it) and unregisters it, with any unclaimed stop, however the run ends.
struct Registration(PathBuf);

impl Registration {
    fn register(common: &Path) -> (Self, Arc<AtomicBool>) {
        // Under the flags lock, the one a stop takes before it records an
        // intent: a stop is either seen here or finds this flag.
        let mut flags = locked(cancels());
        let waiting = locked(stop_intents())
            .remove(common)
            .is_some_and(|asked| asked.elapsed() <= STOP_INTENT_LIFETIME);
        let flag = Arc::new(AtomicBool::new(waiting));
        flags.insert(common.to_path_buf(), Arc::clone(&flag));
        (Self(common.to_path_buf()), flag)
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        locked(cancels()).remove(&self.0);
        locked(stop_intents()).remove(&self.0);
    }
}

/// Whether an apply is running in `common_dir` right now. The Storage panel
/// reads it so a remounted panel shows the run and can stop it.
pub(crate) fn tuning_apply_running(common_dir: &Path) -> bool {
    locked(cancels()).contains_key(common_dir)
}

/// What a click would run right now: the trust check, then a fresh detector
/// pass and [`plan_apply`] over it. An untrusted repository is refused before
/// any read. Does not look at whether an apply is already running, so a second
/// click can join it.
fn plan_now(
    repo_path: &str,
    requested: &[TuningChange],
) -> Result<(PathBuf, PathBuf, TuningPlan), CommandError> {
    let repo = git_cli::validate_repo(repo_path)?;
    crate::repository_trust::require(&repo)?;
    let (private, common) = crate::repository_trust::git_directories(&repo)?;
    let mut advice = crate::storage::tuning_advice_fresh(&repo, &private, &common);
    read_worktree_configs(&mut advice.report);
    let plan = plan_apply(
        &advice.report,
        GitEnv::current(),
        &common.to_string_lossy(),
        requested,
    )
    .map_err(CommandError::from)?;
    Ok((repo, common, plan))
}

/// Most sibling worktrees whose own config is read with git for a split index.
const MAX_WORKTREE_CONFIG_READS: usize = 32;

/// `git config --local` writes the config every worktree shares, so a sibling
/// with `core.splitIndex` in its own `config.worktree` would get
/// `feature.manyFiles` too. The detector spawns nothing and only notes which
/// siblings have such a file; here, once per plan, git reads each one's
/// effective config. A sibling that cannot be read, or more siblings than the
/// bound, is recorded as unverified, which refuses the change.
fn read_worktree_configs(report: &mut RepoTuningReport) {
    let paths = std::mem::take(&mut report.family.with_worktree_config);
    if paths.len() > MAX_WORKTREE_CONFIG_READS {
        report.family.truncated = true;
        return;
    }
    for path in paths {
        match crate::engine::git_reader::config_entries_fresh(Path::new(&path)) {
            Ok(entries) => {
                let split = entries
                    .iter()
                    .rev()
                    .find(|e| e.key == "core.splitindex")
                    .is_some_and(|e| config_true(&e.value));
                if split {
                    report.family.split_index_in.push(path);
                }
            }
            Err(_) => report.family.unverified_indexes.push(path),
        }
    }
}

/// The preview of applying `requested`: exact argv, scope and undo per change,
/// or why it may not be applied. Refused on an untrusted repository, while an
/// apply is already running here, and for anything [`plan_apply`] refuses.
pub fn tuning_plan(
    repo_path: &str,
    requested: &[TuningChange],
) -> Result<TuningPlan, CommandError> {
    let (_repo, common, plan) = plan_now(repo_path, requested)?;
    if tuning_apply_running(&common) {
        return Err(
            "A tuning apply is already running in this repository; stop it from the Storage \
             panel or wait for it to finish"
                .to_string()
                .into(),
        );
    }
    Ok(plan)
}

/// The argvs a confirmed click would run now, for the command layer to judge
/// with the command gate one by one before [`tuning_apply`] spawns any.
pub fn tuning_runnable(
    repo_path: &str,
    requested: &[TuningChange],
) -> Result<Vec<Vec<String>>, CommandError> {
    plan_now(repo_path, requested).map(|(_, _, plan)| plan.runnable())
}

/// Stops the apply running in this repository's common directory. `Ok(true)`
/// when it was running and has been asked to stop; `Ok(false)` when none is
/// registered yet, in which case the stop is remembered for a few seconds and
/// honoured by an apply that registers within it.
pub fn tuning_apply_cancel(repo_path: &str) -> Result<bool, CommandError> {
    let repo = git_cli::validate_repo(repo_path)?;
    let (_, common) = crate::repository_trust::git_directories(&repo)?;
    let flags = locked(cancels());
    if let Some(flag) = flags.get(&common) {
        flag.store(true, Ordering::SeqCst);
        return Ok(true);
    }
    // Recorded under the flags lock, so an apply cannot register between the
    // lookup above and this insert and miss the stop.
    locked(stop_intents()).insert(common, Instant::now());
    Ok(false)
}

/// Applies `requested`, once the person has confirmed the previewed
/// `confirmed` argvs. The command layer has already judged each of them with
/// the command gate; this is the writer, so it checks again that they are what
/// a fresh plan runs and that each is one of the argv shapes it may spawn.
pub fn tuning_apply(
    repo_path: &str,
    requested: &[TuningChange],
    confirmed: &[Vec<String>],
) -> Result<TuningOutcome, CommandError> {
    tuning_apply_within(
        repo_path,
        requested,
        confirmed,
        Duration::from_secs(TUNING_APPLY_TIMEOUT_SECS),
    )
}

pub(crate) fn tuning_apply_within(
    repo_path: &str,
    requested: &[TuningChange],
    confirmed: &[Vec<String>],
    budget: Duration,
) -> Result<TuningOutcome, CommandError> {
    // The claim comes first, before anything is planned: a click for a
    // different set must be refused as "already running", not planned against
    // a repository the running apply is changing.
    let repo = git_cli::validate_repo(repo_path)?;
    crate::repository_trust::require(&repo)?;
    let (_, common_dir) = crate::repository_trust::git_directories(&repo)?;
    let _claim = SetClaim::claim(&common_dir, requested)?;
    let (repo, common, plan) = plan_now(repo_path, requested)?;
    let runnable = plan.runnable();
    if confirmed != runnable.as_slice() {
        return Err(format!(
            "The confirmed commands are not the previewed ones; nothing ran. Previewed: {}",
            render(&runnable)
        )
        .into());
    }
    if let Some(bad) = runnable.iter().find(|argv| !is_apply_argv(argv)) {
        return Err(format!("`{}` is not a tuning command; nothing ran", bad.join(" ")).into());
    }
    let ran = std::cell::Cell::new(false);
    let mut answer = flights().run(common.clone(), || {
        ran.set(true);
        run(&repo, &common, requested, &runnable, budget)
    });
    if let (false, Ok(outcome)) = (ran.get(), answer.as_mut()) {
        outcome.joined = true;
    }
    answer
}

fn render(argvs: &[Vec<String>]) -> String {
    if argvs.is_empty() {
        return "(nothing to run)".to_string();
    }
    argvs
        .iter()
        .map(|argv| argv.join(" "))
        .collect::<Vec<_>>()
        .join("; ")
}

/// Runs the confirmed steps in order under one deadline. A click that queued
/// behind a finished apply re-plans against the repository as it is now, so a
/// change already made is skipped, never repeated.
fn run(
    repo: &Path,
    common: &Path,
    requested: &[TuningChange],
    confirmed: &[Vec<String>],
    budget: Duration,
) -> Flight {
    let started = Instant::now();
    let (_registered, flag) = Registration::register(common);
    let (_, _, plan) = plan_now(&repo.to_string_lossy(), requested)?;
    let mut results: Vec<TuningStepResult> = Vec::new();
    let mut stopped = false;
    let mut failed = false;
    for step in &plan.steps {
        if let Some(why) = &step.skipped {
            results.push(TuningStepResult {
                change: step.change,
                argv: Vec::new(),
                status: TuningStepStatus::Skipped,
                detail: Some(why.clone()),
            });
            continue;
        }
        // Whatever the fresh plan runs must be something the person confirmed.
        if !confirmed.contains(&step.argv) || !is_apply_argv(&step.argv) {
            return Err(format!(
                "`{}` was not among the confirmed commands; nothing further ran",
                step.argv.join(" ")
            )
            .into());
        }
        if failed || flag.load(Ordering::SeqCst) {
            stopped = stopped || flag.load(Ordering::SeqCst);
            results.push(TuningStepResult {
                change: step.change,
                argv: step.argv.clone(),
                status: TuningStepStatus::NotRun,
                detail: None,
            });
            continue;
        }
        let left = budget.saturating_sub(started.elapsed());
        if left.is_zero() {
            failed = true;
            results.push(TuningStepResult {
                change: step.change,
                argv: step.argv.clone(),
                status: TuningStepStatus::Failed,
                detail: Some(format!(
                    "the {TUNING_APPLY_TIMEOUT_SECS} s limit was spent before this step started"
                )),
            });
            record_clicked_write(
                repo,
                common,
                &step.argv,
                "tuning.apply",
                false,
                "timed_out",
                started.elapsed(),
            );
            continue;
        }
        let step_started = Instant::now();
        let args: Vec<&str> = step.argv[1..].iter().map(String::as_str).collect();
        let run = git_cli::git_observed(
            repo,
            &args,
            left,
            OUTPUT_CAP,
            &mut Cancel(Arc::clone(&flag)),
        );
        let (status, ended, detail) = match run {
            Ok(run) if run.cancelled && run.status_code != 0 => {
                stopped = true;
                (
                    TuningStepStatus::NotRun,
                    "cancelled",
                    Some("stopped before git finished".to_string()),
                )
            }
            Ok(git_cli::BoundedRun {
                incomplete: Some(git_cli::Incomplete::Deadline { message, .. }),
                ..
            }) => {
                failed = true;
                (TuningStepStatus::Failed, "timed_out", Some(message))
            }
            Ok(run) if run.success => (TuningStepStatus::Applied, "applied", None),
            Ok(run) => {
                failed = true;
                (
                    TuningStepStatus::Failed,
                    "failed",
                    Some(format!(
                        "{} failed: {}",
                        step.argv.join(" "),
                        git_cli::byte_tail(&run.stderr, 400).trim()
                    )),
                )
            }
            Err(error)
                if flag.load(Ordering::SeqCst) && !git_cli::is_timed_out(error.message()) =>
            {
                stopped = true;
                (
                    TuningStepStatus::NotRun,
                    "cancelled",
                    Some("stopped before git started".to_string()),
                )
            }
            Err(error) => {
                failed = true;
                let ended = if git_cli::is_timed_out(error.message()) {
                    "timed_out"
                } else {
                    "failed"
                };
                (
                    TuningStepStatus::Failed,
                    ended,
                    Some(error.message().to_string()),
                )
            }
        };
        record_clicked_write(
            repo,
            common,
            &step.argv,
            "tuning.apply",
            status == TuningStepStatus::Applied,
            ended,
            step_started.elapsed(),
        );
        results.push(TuningStepResult {
            change: step.change,
            argv: step.argv.clone(),
            status,
            detail,
        });
    }
    let (private, _) = crate::repository_trust::git_directories(repo)?;
    let mut advice = crate::storage::tuning_advice_fresh(repo, &private, common);
    advice.apply_in_flight = false;
    Ok(TuningOutcome {
        cancelled: stopped,
        joined: false,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        results,
        advice,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_click_joins_only_the_same_set_of_changes() {
        let common = PathBuf::from("/claim-test/one/.git");
        let first = SetClaim::claim(
            &common,
            &[TuningChange::UntrackedCache, TuningChange::Maintenance],
        )
        .expect("the first click claims the repository");
        // The same set, in another order and with a repeat, joins.
        let joined = SetClaim::claim(
            &common,
            &[
                TuningChange::Maintenance,
                TuningChange::UntrackedCache,
                TuningChange::Maintenance,
            ],
        )
        .expect("an identical set joins");
        // A different set, larger or smaller, is refused rather than dropped.
        for other in [
            vec![TuningChange::UntrackedCache],
            vec![TuningChange::Fsmonitor],
            TuningChange::ALL.to_vec(),
        ] {
            let refused = SetClaim::claim(&common, &other).err().expect("refused");
            assert!(
                refused.message().contains("already running"),
                "{other:?}: {}",
                refused.message()
            );
        }
        // Another repository is unaffected.
        let elsewhere = PathBuf::from("/claim-test/two/.git");
        assert!(SetClaim::claim(&elsewhere, &[TuningChange::Fsmonitor]).is_ok());
        // The claim lasts until every holder is gone.
        drop(first);
        assert!(SetClaim::claim(&common, &[TuningChange::Fsmonitor]).is_err());
        drop(joined);
        assert!(SetClaim::claim(&common, &[TuningChange::Fsmonitor]).is_ok());
    }
}
