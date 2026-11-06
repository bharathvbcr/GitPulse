//! The tuning apply, run for real against repositories git wrote.
//!
//! `feature.manyFiles` is the change exercised end to end (a repository above
//! its threshold, built with `git fast-import`): the preview, the write, the
//! idempotent second run, the refusals that must leave the config untouched,
//! and a stop. `git maintenance start` is deliberately NOT run here: it installs
//! a per-user launchd / cron / systemd job, which a test must not do to the
//! machine that runs it. Its argv, scope and undo are covered by the plan tests
//! in `storage::repo_tuning::advice_tests`.

mod common;
use common::run_git;
use gitpulse_lib::engine::tuning_apply::{tuning_apply, tuning_apply_cancel, tuning_plan};
use gitpulse_lib::procguard::LockedSpawn;
use gitpulse_lib::storage::repo_tuning::{TuningChange, TuningStepStatus};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Mutex, MutexGuard};

static SERIAL: Mutex<()> = Mutex::new(());

fn serial() -> MutexGuard<'static, ()> {
    common::unmetered_spawns();
    SERIAL
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// A trusted repository with `files` tracked files in one commit.
fn repo_with_files(files: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "gc.auto", "0"]);
    import(dir.path(), files);
    dir
}

fn import(dir: &Path, files: usize) {
    let mut stream =
        String::from("commit refs/heads/main\ncommitter t <t@x> 1700000000 +0000\ndata 1\nc\n");
    for i in 0..files {
        let body = format!("file {i}\n");
        stream.push_str(&format!(
            "M 100644 inline d{}/f{i}.txt\ndata {}\n{body}",
            i % 50,
            body.len()
        ));
    }
    stream.push('\n');
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(dir)
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("spawn fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    run_git(dir, &["reset", "-q", "--hard", "main"]);
}

fn local_value(dir: &Path, key: &str) -> Option<String> {
    let out = Command::new("git")
        .args(["config", "--local", "--get", key])
        .current_dir(dir)
        .output_locked()
        .unwrap();
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

const ABOVE: usize = 20_500;

#[test]
fn a_repository_below_every_threshold_is_offered_and_applied_nothing() {
    let _g = serial();
    let dir = repo_with_files(50);
    let path = dir.path().to_str().unwrap();
    for change in [
        TuningChange::ManyFiles,
        TuningChange::Fsmonitor,
        TuningChange::Maintenance,
    ] {
        let refused = tuning_plan(path, &[change]).unwrap_err();
        assert!(
            refused.message().contains("not offered"),
            "{change:?}: {}",
            refused.message()
        );
        assert!(tuning_apply(path, &[change], &[]).is_err());
    }
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
}

#[test]
fn an_untrusted_repository_is_refused_before_anything_is_read_or_run() {
    let _g = serial();
    let dir = tempfile::tempdir().unwrap();
    // `common::run_git` trusts on init; this fixture is deliberately not.
    let init = Command::new("git")
        .args(["init", "-q", "-b", "main"])
        .current_dir(dir.path())
        .output_locked()
        .unwrap();
    assert!(init.status.success());
    let path = dir.path().to_str().unwrap();
    let plan = tuning_plan(path, &[TuningChange::ManyFiles]).unwrap_err();
    assert!(
        plan.message().to_lowercase().contains("trust"),
        "{}",
        plan.message()
    );
    let apply = tuning_apply(
        path,
        &[TuningChange::ManyFiles],
        &[vec![
            "git".into(),
            "config".into(),
            "--local".into(),
            "feature.manyFiles".into(),
            "true".into(),
        ]],
    )
    .unwrap_err();
    assert!(
        apply.message().to_lowercase().contains("trust"),
        "{}",
        apply.message()
    );
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
}

#[test]
fn many_files_is_previewed_applied_once_and_skipped_the_second_time() {
    let _g = serial();
    let dir = repo_with_files(ABOVE);
    let path = dir.path().to_str().unwrap();

    let plan = tuning_plan(path, &[TuningChange::ManyFiles]).expect("offered above its threshold");
    assert_eq!(plan.steps.len(), 1);
    let step = &plan.steps[0];
    assert_eq!(
        step.argv,
        ["git", "config", "--local", "feature.manyFiles", "true"]
    );
    assert_eq!(
        step.undo,
        [vec![
            "git",
            "config",
            "--local",
            "--unset",
            "feature.manyFiles"
        ]]
    );
    // Previewing writes nothing.
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);

    let outcome = tuning_apply(path, &[TuningChange::ManyFiles], &plan.runnable()).expect("apply");
    assert_eq!(outcome.results.len(), 1);
    assert_eq!(
        outcome.results[0].status,
        TuningStepStatus::Applied,
        "{outcome:?}"
    );
    assert!(!outcome.cancelled && !outcome.joined);
    assert_eq!(
        local_value(dir.path(), "feature.manyFiles").as_deref(),
        Some("true")
    );
    // The report that comes back is read fresh: it already sees the key.
    assert_eq!(
        outcome
            .advice
            .report
            .key("feature.manyfiles")
            .unwrap()
            .value
            .as_deref(),
        Some("true")
    );
    assert!(
        outcome.advice.offers.is_empty(),
        "nothing left to offer: {:?}",
        outcome.advice.offers
    );

    // Idempotent: the second plan skips it, runs nothing, and says why.
    let again = tuning_plan(path, &[TuningChange::ManyFiles]).expect("a plan, not an error");
    assert!(again.runnable().is_empty());
    assert!(again.steps[0]
        .skipped
        .as_deref()
        .unwrap()
        .contains("already on"));
    let second = tuning_apply(path, &[TuningChange::ManyFiles], &[]).expect("apply");
    assert_eq!(second.results[0].status, TuningStepStatus::Skipped);

    // And the undo the preview named restores the repository.
    run_git(
        dir.path(),
        &["config", "--local", "--unset", "feature.manyFiles"],
    );
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
}

#[test]
fn a_confirmed_list_that_is_not_the_preview_runs_nothing() {
    let _g = serial();
    let dir = repo_with_files(ABOVE);
    let path = dir.path().to_str().unwrap();
    let hostile: [Vec<Vec<String>>; 4] = [
        vec![],
        vec![["git", "config", "--local", "core.hooksPath", "/evil"]
            .map(String::from)
            .to_vec()],
        vec![["git", "config", "--global", "feature.manyFiles", "true"]
            .map(String::from)
            .to_vec()],
        vec![
            ["git", "config", "--local", "feature.manyFiles", "true"]
                .map(String::from)
                .to_vec(),
            ["git", "config", "--local", "core.hooksPath", "/evil"]
                .map(String::from)
                .to_vec(),
        ],
    ];
    for confirmed in hostile {
        let refused = tuning_apply(path, &[TuningChange::ManyFiles], &confirmed).unwrap_err();
        assert!(
            refused.message().contains("not the previewed"),
            "{}",
            refused.message()
        );
    }
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
    assert_eq!(local_value(dir.path(), "core.hooksPath"), None);
}

#[test]
fn many_files_over_a_split_index_is_refused_and_nothing_is_written() {
    let _g = serial();
    let dir = repo_with_files(ABOVE);
    run_git(
        dir.path(),
        &["config", "--local", "core.splitIndex", "true"],
    );
    let path = dir.path().to_str().unwrap();
    let refused = tuning_plan(path, &[TuningChange::ManyFiles]).unwrap_err();
    assert!(
        refused.message().contains("add-then-commit"),
        "{}",
        refused.message()
    );
    assert!(tuning_apply(path, &[TuningChange::ManyFiles], &[]).is_err());
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
}

#[test]
fn a_stop_that_arrives_first_leaves_the_step_unrun() {
    let _g = serial();
    let dir = repo_with_files(ABOVE);
    let path = dir.path().to_str().unwrap();
    let plan = tuning_plan(path, &[TuningChange::ManyFiles]).unwrap();
    // No apply is registered yet: the stop is remembered and honoured by the
    // one that registers next.
    assert!(!tuning_apply_cancel(path).unwrap());
    let outcome = tuning_apply(path, &[TuningChange::ManyFiles], &plan.runnable()).unwrap();
    assert!(outcome.cancelled, "{outcome:?}");
    assert_eq!(outcome.results[0].status, TuningStepStatus::NotRun);
    assert_eq!(local_value(dir.path(), "feature.manyFiles"), None);
    // The stop was consumed: the next apply runs.
    let outcome = tuning_apply(path, &[TuningChange::ManyFiles], &plan.runnable()).unwrap();
    assert_eq!(outcome.results[0].status, TuningStepStatus::Applied);
}
