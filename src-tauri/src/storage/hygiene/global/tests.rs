use super::{
    eligible_candidates, Cleaner, CleanerCandidate, CleanerConfig, CleanerRun, Saved, VERSION,
};
use crate::storage::hygiene::discovery;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<Cleaner>, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
    let code = root.join("code");
    fs::create_dir(&code).unwrap();
    let cleaner = Arc::new(Cleaner::new(root.join("state")));
    (temp, cleaner, code)
}
fn config(cleaner: &Cleaner, root: &std::path::Path) -> CleanerConfig {
    let mut config = cleaner.state().unwrap().config;
    config.roots = vec![root.to_string_lossy().into_owned()];
    config
}
fn repo(root: &std::path::Path, name: &str) -> std::path::PathBuf {
    let repo = root.join(name);
    fs::create_dir_all(&repo).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    crate::test_support::trust_repo(&repo);
    repo
}

/// Makes a directory unreadable and restores its mode when the guard drops.
///
/// Every permission-denial test here runs real work with a directory at mode 0
/// -- `save`, `update`, `lock`, `inventory` or `perform_with_gate` and its
/// `debug_assert!` -- and a panic anywhere in that window skips a hand-written
/// restore. `TempDir::drop` then cannot remove the tree, so a failing test
/// leaks its fixture under `$TMPDIR` on top of failing. Tests that need to read
/// through the directory in their assertions still call [`Denied::restore`]
/// first; this only guarantees the mode comes back on the panic path, and a
/// second restore is harmless.
#[cfg(unix)]
#[must_use = "the directory is restored when this guard drops"]
struct Denied(std::path::PathBuf);

#[cfg(unix)]
fn deny(path: &std::path::Path) -> Denied {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o0)).unwrap();
    Denied(path.to_path_buf())
}

#[cfg(unix)]
impl Denied {
    /// Restores the mode now, for assertions that have to read inside.
    fn restore(&self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[cfg(unix)]
impl Drop for Denied {
    fn drop(&mut self) {
        self.restore();
    }
}
#[test]
fn global_policy_defaults_to_no_schedule_and_rejects_unbounded_inputs() {
    let c = CleanerConfig::default();
    assert!(!c.enabled);
    assert!(c.roots.is_empty());
    assert!(c.validate().is_ok());
    for days in [0, 1, 6, 3651, u32::MAX] {
        let mut v = c.clone();
        v.retention_days = days;
        assert!(v.validate().is_err());
    }
    for count in [0, 101, u32::MAX] {
        let mut v = c.clone();
        v.max_targets = count;
        assert!(v.validate().is_err());
    }
    for bytes in [0, u64::MAX] {
        let mut v = c.clone();
        v.max_run_bytes = bytes;
        assert!(v.validate().is_err());
    }
    let mut v = c.clone();
    v.version = VERSION + 1;
    assert!(v.validate().is_err());
    v = c.clone();
    v.enabled = true;
    assert!(v.validate().is_err());
}
#[test]
fn stale_settings_cannot_erase_another_windows_policy() {
    let (_temp, cleaner, root) = fixture();
    let config = config(&cleaner, &root);
    let saved = cleaner.save(config.clone()).unwrap();
    assert_eq!(saved.config.revision, 1);
    assert!(cleaner.save(config).unwrap_err().contains("another window"));
    assert_eq!(
        cleaner.state().unwrap().config.roots,
        vec![root.to_string_lossy()]
    );
}
#[test]
fn corrupt_future_and_oversized_state_never_enable_a_default_schedule() {
    let (_temp, cleaner, _root) = fixture();
    cleaner.ensure_dir().unwrap();
    for bytes in [b"garbage".to_vec(), vec![b'x'; 1024 * 1024 + 1]] {
        fs::write(cleaner.dir.join("state.json"), bytes).unwrap();
        assert!(cleaner.state().is_err());
    }
    let mut future = Saved::default();
    future.config.version = VERSION + 1;
    fs::write(
        cleaner.dir.join("state.json"),
        serde_json::to_vec(&future).unwrap(),
    )
    .unwrap();
    assert!(cleaner.state().is_err());
}
#[test]
fn discovery_deduplicates_overlapping_roots_preserves_exclusions_and_finds_closed_repos() {
    let (_temp, _cleaner, root) = fixture();
    let a = repo(&root, "a");
    let b = repo(&root, "nested/b");
    let inputs = vec![
        root.to_string_lossy().into_owned(),
        a.to_string_lossy().into_owned(),
        root.to_string_lossy().into_owned(),
    ];
    let roots = discovery::roots(&inputs, false).unwrap();
    assert_eq!(roots, vec![root.clone()]);
    let report = discovery::discover(&roots, std::slice::from_ref(&b), &AtomicBool::new(false));
    assert_eq!(report.repos.into_iter().collect::<Vec<_>>(), vec![a]);
    assert!(!report.partial);
    assert!(discovery::discover(&roots, &[], &AtomicBool::new(true)).partial);
}

/// GitHub Windows CI 34441565936: `validate_repo` kept `\\?\`, the walk used
/// `canonicalize_plain`, and `root == path` reported every repo as a mismatch.
#[test]
fn discovery_accepts_a_repo_whose_validate_repo_root_is_verbatim() {
    let (_temp, _cleaner, root) = fixture();
    let walked = crate::engine::git_cli::canonicalize_plain(&repo(&root, "a")).unwrap();
    let validated = crate::engine::git_cli::validate_repo(walked.to_str().unwrap()).unwrap();
    #[cfg(windows)]
    {
        assert!(
            validated.to_string_lossy().starts_with(r"\\?\"),
            "Windows validate_repo must keep the verbatim prefix; otherwise this no longer covers the CI failure: {validated:?}"
        );
        assert_ne!(validated, walked);
    }
    assert!(
        discovery::same_discovered_repo(&validated, &walked),
        "validated={validated:?} walked={walked:?}"
    );
    let report = discovery::discover(std::slice::from_ref(&root), &[], &AtomicBool::new(false));
    assert!(
        report.repos.contains(&walked),
        "repos={:?} issues={:?} validated={validated:?}",
        report.repos,
        report.issues
    );
    assert!(!report.partial, "issues={:?}", report.issues);
}
#[test]
fn depth_and_repository_caps_are_explicit_partial_results() {
    let (_temp, _cleaner, root) = fixture();
    let mut nested = root.clone();
    for _ in 0..26 {
        nested.push("deep");
    }
    fs::create_dir_all(nested).unwrap();
    let result = discovery::discover(std::slice::from_ref(&root), &[], &AtomicBool::new(false));
    assert!(result.partial);
    assert!(!result.issues.is_empty());
}
#[test]
fn cross_instance_locks_are_exclusive_and_release_on_drop() {
    let (_temp, first, _root) = fixture();
    let second = Cleaner::new(first.dir.clone());
    let held = first.lock("run.lock").unwrap();
    assert!(second.state().unwrap().running);
    assert!(second.lock("run.lock").is_err());
    drop(held);
    assert!(!second.state().unwrap().running);
    assert!(second.lock("run.lock").is_ok());
}
#[test]
fn restart_reports_unfinished_history_as_interrupted_without_replaying_it() {
    let (_temp, cleaner, _root) = fixture();
    cleaner
        .update(|saved| {
            saved.history.push(CleanerRun {
                id: "old".into(),
                trigger: "scheduled".into(),
                started_at: 1,
                finished_at: 0,
                status: "running".into(),
                repositories: 0,
                partial: false,
                issues: vec![],
                items: vec![],
                unlisted: 0,
                unexamined: 0,
            });
            Ok(())
        })
        .unwrap();
    let state = cleaner.state().unwrap();
    assert!(!state.running);
    assert_eq!(state.history[0].status, "interrupted");
}
#[test]
fn missing_or_replaced_lock_is_unavailable_not_a_running_job() {
    let (_temp, cleaner, _root) = fixture();
    cleaner.ensure_dir().unwrap();
    fs::create_dir(cleaner.dir.join("run.lock")).unwrap();
    assert!(cleaner.state().is_err());
}
#[cfg(unix)]
#[test]
fn a_disabled_or_future_schedule_cannot_start_or_mutate_history() {
    let (_temp, cleaner, root) = fixture();
    let mut c = config(&cleaner, &root);
    c.enabled = true;
    c.next_run_at = crate::storage::hygiene::now() + 3600;
    let state = cleaner.save(c).unwrap();
    assert!(cleaner
        .start(true, state.config.revision, crate::storage::hygiene::now())
        .is_err());
    assert!(cleaner.state().unwrap().history.is_empty());
    let mut c = cleaner.state().unwrap().config;
    c.enabled = false;
    let state = cleaner.save(c).unwrap();
    assert!(cleaner
        .start(true, state.config.revision, u64::MAX)
        .is_err());
    assert!(!cleaner.active.load(std::sync::atomic::Ordering::SeqCst));
}
#[cfg(not(unix))]
#[test]
fn enabled_schedules_are_refused_on_this_platform() {
    let (_temp, cleaner, root) = fixture();
    let mut c = config(&cleaner, &root);
    c.enabled = true;
    c.next_run_at = crate::storage::hygiene::now() + 3600;
    assert!(cleaner
        .save(c)
        .unwrap_err()
        .contains("unavailable on this platform"));
}
#[test]
fn external_cancellation_and_policy_edits_revoke_authority() {
    let (_temp, first, root) = fixture();
    let second = Cleaner::new(first.dir.clone());
    let saved = first.save(config(&first, &root)).unwrap();
    first
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    assert!(first.still_authorized(&saved.config, "run").is_ok());
    second
        .update(|s| {
            s.cancel_run = Some("run".into());
            Ok(())
        })
        .unwrap();
    assert!(first.still_authorized(&saved.config, "run").is_err());
}
#[cfg(unix)]
#[test]
fn symlink_roots_state_and_discovery_escape_are_refused() {
    use std::os::unix::fs::symlink;
    let (_temp, cleaner, root) = fixture();
    let outside = tempfile::tempdir().unwrap();
    let other = repo(outside.path(), "outside");
    symlink(&other, root.join("escape")).unwrap();
    assert!(
        discovery::roots(&[root.join("escape").to_string_lossy().into_owned()], false).is_err()
    );
    assert!(discovery::discover(&[root], &[], &AtomicBool::new(false))
        .repos
        .is_empty());
    cleaner.ensure_dir().unwrap();
    symlink(
        outside.path().join("precious"),
        cleaner.dir.join("state.json"),
    )
    .unwrap();
    assert!(cleaner.state().is_err());
    assert!(!outside.path().join("precious").exists());
}
#[test]
fn dirty_repositories_and_unreadable_task_state_are_refused() {
    let (_temp, _cleaner, root) = fixture();
    let r = repo(&root, "repo");
    fs::write(r.join("source"), "working").unwrap();
    assert!(super::quiet_repo(r.to_str().unwrap())
        .unwrap_err()
        .contains("uncommitted"));
    fs::remove_file(r.join("source")).unwrap();
    fs::write(r.join(".git/info/exclude"), ".devcouncil/\n").unwrap();
    fs::create_dir(r.join(".devcouncil")).unwrap();
    fs::write(r.join(".devcouncil/state.sqlite"), "broken database").unwrap();
    assert!(super::quiet_repo(r.to_str().unwrap())
        .unwrap_err()
        .contains("Task activity"));
}

#[cfg(unix)]
fn wait(cleaner: &Cleaner) -> super::CleanerState {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let state = cleaner.state().unwrap();
        if !state.running {
            return state;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "worker did not finish within its test budget"
        );
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}
#[test]
fn a_worker_finishing_during_a_status_read_is_never_reported_interrupted() {
    let (_temp, worker, _root) = fixture();
    let lease = worker.lock("run.lock").unwrap();
    worker
        .update(|saved| {
            saved.history.push(CleanerRun {
                id: "finishing-worker".into(),
                trigger: "manual".into(),
                started_at: 1,
                finished_at: 0,
                status: "running".into(),
                repositories: 0,
                partial: false,
                issues: vec![],
                items: vec![],
                unlisted: 0,
                unexamined: 0,
            });
            Ok(())
        })
        .unwrap();
    let mut observer = Cleaner::new(worker.dir.clone());
    let lease = std::sync::Mutex::new(Some(lease));
    observer.state_after_read = Some(Box::new(move || {
        // Force the real worker completion order at the stale-snapshot seam.
        // Separate Cleaner instances also exercise cross-process lock authority.
        if let Some(lease) = lease.lock().unwrap().take() {
            worker
                .update(|saved| {
                    saved.history[0].finished_at = 2;
                    saved.history[0].status = "completed".into();
                    Ok(())
                })
                .unwrap();
            drop(lease);
        }
    }));
    let overlapping = observer.state().unwrap();
    assert!(
        overlapping.running || overlapping.history[0].status == "completed",
        "a successful overlapping completion was misclassified: {overlapping:?}"
    );
    assert_ne!(overlapping.history[0].status, "interrupted");
    let settled = observer.state().unwrap();
    assert!(!settled.running);
    assert_eq!(settled.history[0].finished_at, 2);
    assert_eq!(settled.history[0].status, "completed");
}
#[cfg(unix)]
#[test]
fn missed_schedule_runs_once_and_advances_before_execution() {
    let (_temp, cleaner, root) = fixture();
    let mut c = config(&cleaner, &root);
    let now = crate::storage::hygiene::now();
    c.enabled = true;
    c.next_run_at = now - 30 * 86400;
    c.interval_hours = 24;
    let saved = cleaner.save(c).unwrap();
    let original = saved.config.clone();
    cleaner.start(true, original.revision, now).unwrap();
    let done = wait(&cleaner);
    assert_eq!(done.config.next_run_at, now + 86400);
    assert_eq!(done.history.len(), 1);
    assert_eq!(done.history[0].status, "completed");
    assert!(done.history[0].items.is_empty());
    assert!(cleaner.start(true, done.config.revision, now).is_err());
    assert!(
        cleaner.save(original).is_err(),
        "a stale window cannot restore the old due time"
    );
}
#[cfg(unix)]
#[test]
fn concurrent_schedule_claims_have_one_durable_winner() {
    let (_temp, cleaner, root) = fixture();
    let now = crate::storage::hygiene::now();
    let mut c = config(&cleaner, &root);
    c.enabled = true;
    c.next_run_at = now;
    let saved = cleaner.save(c).unwrap();
    let barrier = Arc::new(std::sync::Barrier::new(16));
    let threads: Vec<_> = (0..16)
        .map(|_| {
            let c = Arc::clone(&cleaner);
            let b = Arc::clone(&barrier);
            let rev = saved.config.revision;
            std::thread::spawn(move || {
                b.wait();
                c.start(true, rev, now).is_ok()
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .filter_map(|t| t.join().ok())
            .filter(|won| *won)
            .count(),
        1
    );
    assert_eq!(wait(&cleaner).history.len(), 1);
}
#[cfg(unix)]
#[test]
fn manual_runs_keep_bounded_history_and_never_enable_scheduling() {
    let (_temp, cleaner, root) = fixture();
    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    for i in 0..24 {
        cleaner
            .start(
                false,
                saved.config.revision,
                crate::storage::hygiene::now() + i,
            )
            .unwrap();
        let state = wait(&cleaner);
        assert!(!state.config.enabled);
        assert!(state.history.len() <= 20);
        assert!(
            state.history.iter().all(|r| r.status == "completed"),
            "unexpected run history: {:?}",
            state.history
        );
    }
    assert_eq!(cleaner.state().unwrap().history.len(), 20);
}

#[cfg(unix)]
#[test]
fn global_cleanup_uses_real_snapshots_and_removal_with_exact_limits() {
    // Only OS process activity is injected: the test itself runs under Cargo.
    // Discovery, Git status/index, journal and removal are real. The policy
    // service is injected at the same gate called by production.
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("execution-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    for name in ["a", "b"] {
        let r = repo(&root, name);
        fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
        fs::write(r.join("source.py"), "print('preserve')").unwrap();
        crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
        crate::engine::git_cli::git_text(
            &r,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        )
        .unwrap();
        fs::create_dir(r.join("__pycache__")).unwrap();
        fs::write(r.join("__pycache__/module.pyc"), vec![b'x'; 100]).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
        for p in [r.join("__pycache__/module.pyc"), r.join("__pycache__")] {
            fs::File::open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
    }
    let mut c = config(&cleaner, &root);
    c.max_run_bytes = 100;
    c.max_targets = 2;
    let saved = cleaner.save(c).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "fixture-run".into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let mut gated = 0;
    cleaner
        .perform_with_gate(&saved.config, &mut run, |repo, argv, path| {
            assert!(argv.is_none());
            assert_eq!(
                std::path::Path::new(path),
                std::path::Path::new(repo).join("__pycache__")
            );
            let state = cleaner.read().unwrap();
            assert_eq!(state.history[0].items.last().unwrap().status, "running");
            gated += 1;
            Ok(())
        })
        .unwrap();
    assert_eq!(gated, 1);
    assert_eq!(
        run.items.iter().filter(|r| r.status == "completed").count(),
        1,
        "{run:?}"
    );
    assert_eq!(
        run.items.iter().filter(|r| r.status == "skipped").count(),
        1,
        "{run:?}"
    );
    assert_eq!(
        ["a", "b"]
            .into_iter()
            .filter(|name| root.join(name).join("__pycache__").exists())
            .count(),
        1
    );
    assert!(root.join("a/source.py").is_file());
    assert!(root.join("b/source.py").is_file());
    assert!(run.items.iter().any(|r| r.message.contains("byte budget")));
}

#[cfg(unix)]
#[test]
fn global_status_never_invokes_a_repository_fsmonitor_hook() {
    use std::os::unix::fs::PermissionsExt;
    let (_temp, _cleaner, root) = fixture();
    let r = repo(&root, "monitored");
    fs::write(r.join("tracked"), "source").unwrap();
    crate::engine::git_cli::git_text(&r, &["add", "tracked"]).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();
    let hook = r.join(".git/test-fsmonitor");
    fs::write(
        &hook,
        "#!/bin/sh\nprintf invoked > .git/fsmonitor-was-invoked\n",
    )
    .unwrap();
    fs::set_permissions(&hook, fs::Permissions::from_mode(0o700)).unwrap();
    crate::engine::git_cli::git_text(&r, &["config", "core.fsmonitor", hook.to_str().unwrap()])
        .unwrap();
    super::quiet_repo(r.to_str().unwrap()).unwrap();
    fs::write(r.join(".git/info/exclude"), "__pycache__/\n").unwrap();
    fs::create_dir(r.join("__pycache__")).unwrap();
    fs::write(r.join("__pycache__/a.pyc"), "bytecode").unwrap();
    crate::storage::scan_storage(r.to_str().unwrap()).unwrap();
    crate::storage::hygiene::local_checks(&r, "__pycache__").unwrap();
    assert!(!r.join(".git/fsmonitor-was-invoked").exists());
}

#[cfg(unix)]
#[test]
fn background_registration_failure_disables_durable_authority_and_surfaces_recovery() {
    let (_temp, cleaner, root) = fixture();
    let mut policy = config(&cleaner, &root);
    policy.enabled = true;
    policy.run_when_closed = true;
    policy.next_run_at = crate::storage::hygiene::now() + 3600;
    let error = cleaner
        .save_with_background(policy, |enabled| {
            assert!(enabled);
            assert!(!cleaner.read().unwrap().config.enabled);
            Err("launchd unavailable".into())
        })
        .unwrap_err();
    assert!(error.contains("Schedule disabled"));
    let failed = cleaner.state().unwrap();
    assert!(!failed.config.enabled && !failed.config.run_when_closed);
    assert_eq!(
        failed.background_error.as_deref(),
        Some("launchd unavailable")
    );
    let repaired = cleaner
        .save_with_background(failed.config, |enabled| {
            assert!(!enabled);
            Ok(())
        })
        .unwrap();
    assert!(repaired.background_error.is_none());
    assert!(!repaired.config.enabled);
}

#[cfg(unix)]
#[test]
fn turning_off_background_mode_revokes_before_unloading_and_plain_saves_do_not_register_jobs() {
    let (_temp, cleaner, root) = fixture();
    cleaner
        .save_with_background(config(&cleaner, &root), |_| {
            panic!("default-off save must not touch launchd")
        })
        .unwrap();
    let mut policy = cleaner.state().unwrap().config;
    policy.enabled = true;
    policy.run_when_closed = true;
    policy.next_run_at = crate::storage::hygiene::now() + 3600;
    cleaner.save_with_background(policy, |_| Ok(())).unwrap();
    let mut policy = cleaner.state().unwrap().config;
    policy.enabled = false;
    cleaner
        .save_with_background(policy, |enabled| {
            assert!(!enabled);
            assert!(!cleaner.read().unwrap().config.enabled);
            Ok(())
        })
        .unwrap();
}

#[cfg(unix)]
#[test]
fn policy_edits_inside_the_execution_gate_preserve_real_artifacts() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, mut cleaner, root) = fixture();
    Arc::get_mut(&mut cleaner).unwrap().activity_override = Some(|_| Ok(()));
    let r = repo(&root, "repo");
    fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
    crate::engine::git_cli::git_text(&r, &["add", ".gitignore"]).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();
    let cache = r.join("__pycache__");
    fs::create_dir(&cache).unwrap();
    fs::write(cache.join("a.pyc"), "bytecode").unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
    for path in [cache.clone(), cache.join("a.pyc")] {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }
    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "revoked-run".into(),
        trigger: "manual".into(),
        started_at: 1,
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let other = Cleaner::new(cleaner.dir.clone());
    let mut gated = false;
    cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| {
            gated = true;
            let mut policy = other.read().unwrap().config;
            policy.exclusions = vec![cache.to_string_lossy().into_owned()];
            other.save(policy)?;
            Ok(())
        })
        .unwrap();
    assert!(gated);
    assert!(cache.join("a.pyc").is_file());
    assert_eq!(run.items[0].status, "skipped");
    assert!(run.items[0].message.contains("settings changed"));
}

#[test]
fn incomplete_discovery_blocks_every_mutation() {
    let (_temp, cleaner, root) = fixture();
    let mut deep = root.clone();
    for _ in 0..26 {
        deep.push("deep");
    }
    fs::create_dir_all(deep).unwrap();
    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "partial".into(),
        trigger: "manual".into(),
        started_at: 1,
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    assert!(cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| panic!(
            "partial scan cannot mutate"
        ))
        .unwrap_err()
        .contains("Incomplete inventory"));
    assert!(run.partial);
    assert!(run.items.is_empty());
}

#[test]
fn failed_background_setup_with_a_busy_state_writer_still_leaves_authority_disabled() {
    let (_temp, cleaner, root) = fixture();
    let mut policy = config(&cleaner, &root);
    policy.enabled = true;
    policy.run_when_closed = true;
    policy.next_run_at = crate::storage::hygiene::now() + 3600;
    let mut held = None;
    assert!(cleaner
        .save_with_background(policy, |_| {
            held = Some(cleaner.lock("state.lock").unwrap());
            Err("registration failed during a journal write".into())
        })
        .is_err());
    assert!(
        !cleaner.read().unwrap().config.enabled,
        "registration failure must not leave an enabled schedule even when recovery cannot write"
    );
    drop(held);
}

#[test]
fn dropped_locks_release_even_while_a_duplicated_descriptor_is_alive() {
    let (_temp, cleaner, _root) = fixture();
    let held = cleaner.lock("state.lock").unwrap();
    let inherited = held.try_clone().unwrap();
    drop(held);
    assert!(
        cleaner.lock("state.lock").is_ok(),
        "a transient fork/duplicate must not retain the lock after its owner finishes"
    );
    drop(inherited);
}

#[cfg(unix)]
#[test]
fn inaccessible_repository_contents_make_the_global_inventory_incomplete() {
    let (_temp, cleaner, root) = fixture();
    let r = repo(&root, "repo");
    let hidden = r.join(".hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("source"), "preserve").unwrap();
    cleaner.save(config(&cleaner, &root)).unwrap();
    let denied = deny(&hidden);
    let report = cleaner.inventory();
    denied.restore();
    assert!(
        report.unwrap().partial,
        "an inaccessible storage subtree is not a complete scan"
    );
}

/// Eligibility is per artifact tree, and it is the tree's OWN enumeration that
/// decides. A repository is reported partial for reasons that say nothing about
/// any one tree -- linked worktrees past the sizing cap, artifact directories
/// past the reporting cap, a flat source directory past the entry cap -- and
/// reading that flag as "nothing here was examined" disqualified every tree in
/// the repository, including the ones the scan had read to the end. What must
/// still disqualify a tree is that tree being unreadable.
#[cfg(unix)]
#[test]
fn an_unreadable_artifact_tree_is_dropped_while_a_readable_one_stays_a_candidate() {
    let (_temp, cleaner, root) = fixture();
    let r = repo(&root, "repo");
    fs::write(r.join(".gitignore"), "target/\n").unwrap();

    // Two Cargo projects in one repository. `local_provider` only accepts a
    // `target` next to a Cargo.toml that carries Cargo's own markers, so both
    // get them; the sole difference is a subtree one of them cannot read.
    for name in ["whole", "cut"] {
        let project = r.join(name);
        let target = project.join("target");
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(project.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        fs::write(
            target.join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        fs::write(target.join(".rustc_info.json"), "{}").unwrap();
        fs::write(target.join("debug").join("x.o"), vec![b'x'; 4096]).unwrap();
    }
    let blocked = r.join("cut/target/deps");
    fs::create_dir_all(&blocked).unwrap();
    fs::write(blocked.join("lib.rlib"), vec![b'x'; 4096]).unwrap();
    cleaner.save(config(&cleaner, &root)).unwrap();
    let denied = deny(&blocked);

    let inventory = cleaner.inventory();
    denied.restore();
    let inventory = inventory.expect("inventory");
    let paths: Vec<&str> = inventory
        .candidates
        .iter()
        .map(|c| c.path.as_str())
        .collect();

    assert!(
        !paths.contains(&"cut/target"),
        "a tree the walk could not finish is not a removal candidate: {paths:?}"
    );
    assert!(
        paths.contains(&"whole/target"),
        "a tree that WAS read to the end stays eligible even though the \
         repository scan is partial: {paths:?}"
    );
    assert!(
        inventory.partial,
        "the run still says something could not be read"
    );
}

#[test]
fn an_incomplete_repository_does_not_veto_the_complete_ones() {
    let candidate = |repo: &str, path: &str| CleanerCandidate {
        repo_path: repo.into(),
        path: path.into(),
        provider: "cargo".into(),
        bytes: 1,
        scope: super::SCOPE_REPOSITORY.into(),
        private_bytes: None,
        branch: None,
    };
    let candidates = vec![
        candidate("/w/trusted", "target"),
        candidate("/w/untrusted", "target"),
        candidate("/w/partial", "node_modules"),
        candidate("/w/trusted", "node_modules"),
    ];
    let incomplete = ["/w/untrusted".to_string(), "/w/partial".to_string()]
        .into_iter()
        .collect();

    let (eligible, skipped) = eligible_candidates(candidates, &incomplete);

    assert_eq!(skipped, 2, "both artifacts in unread trees must be skipped");
    assert_eq!(
        eligible.len(),
        2,
        "the complete tree's artifacts stay eligible"
    );
    assert!(
        eligible.iter().all(|c| c.repo_path == "/w/trusted"),
        "no candidate from an incompletely read repository may survive: {:?}",
        eligible.iter().map(|c| &c.repo_path).collect::<Vec<_>>()
    );
}

#[test]
fn every_candidate_from_an_unread_repository_is_filtered_out() {
    let candidates = vec![CleanerCandidate {
        repo_path: "/w/untrusted".into(),
        path: "target".into(),
        provider: "cargo".into(),
        bytes: 1,
        scope: super::SCOPE_REPOSITORY.into(),
        private_bytes: None,
        branch: None,
    }];
    let incomplete = ["/w/untrusted".to_string()].into_iter().collect();
    let (eligible, skipped) = eligible_candidates(candidates, &incomplete);
    assert!(eligible.is_empty(), "nothing may be cleaned");
    assert_eq!(skipped, 1);
}

/// Completeness is a property of an artifact tree, not of the repository that
/// contains it, and this test asserts the narrower veto that follows.
///
/// It used to assert the opposite: that an unreadable directory anywhere in a
/// repository made every artifact in that repository permanently ineligible.
/// That was a deliberate policy and it is retired on purpose, not loosened to
/// make a test pass. The owner decided the narrower rule after the evidence
/// below; the two halves of it are this test and
/// `an_unreadable_directory_inside_an_artifact_tree_keeps_it_ineligible`.
///
/// Why the sibling cannot change the answer. Nothing the removal rests on is
/// read from the repository-wide walk. `unignored` and `tracked_files` come
/// from `git check-ignore` and `git ls-files` queries scoped to the artifact's
/// own path. The retention stamp is read inside the tree. The producer evidence
/// is a `stat` on a named path -- for this fixture's `__pycache__` there is no
/// marker check at all, and where there is one `local_provider` reads the
/// artifact's *parent* (`Cargo.toml`, `package.json`, `pom.xml` and so on)
/// alongside in-tree markers like `CACHEDIR.TAG`; either way it is a named
/// path, not a walk. The byte figure and the completeness come from the
/// per-artifact budget in `walk_worktree`. A directory elsewhere in the
/// repository is not evidence about any of them.
///
/// What reading the repository flag as "nothing here was examined" cost: 20
/// consecutive daily runs reclaiming 0 bytes across a 57-repository workspace,
/// two repositories untrusted and six truncated by the budget, so `partial` was
/// true on every run and always would be. That figure is `2540db65`'s, measured
/// from the hygiene state file; it is the one on record, and the only one this
/// comment should be quoted for.
///
/// The repository is not left unguarded by this. `quiet_repo` is still a
/// repository-wide gate at removal time, and this test passes only because
/// `.unreadable/` is committed to `.gitignore`: an unreadable directory that
/// was not ignored would show up in `git status --porcelain`, and the
/// repository would be refused for having uncommitted work.
///
/// What still blocks removal is an unread directory *inside* the artifact's own
/// tree, because then the walk genuinely cannot say what is in there and the
/// size is a floor. The companion test named above covers that half, and
/// `an_unreadable_artifact_tree_is_dropped_while_a_readable_one_stays_a_candidate`
/// covers it at the inventory level.
///
/// This still covers the call site in `perform_with_gate`, which is where the
/// refusal used to be global: restoring `if run.partial` there makes this test
/// fail, because the run would refuse outright rather than clean either tree.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_beside_an_artifact_tree_does_not_veto_it() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("mixed-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);

    // Both repositories carry an identical, long-stale __pycache__ with the
    // same producer evidence. The only difference is that "unread" also holds
    // a directory the storage scan cannot enter -- a sibling of the artifact,
    // not part of it -- which makes that repository's scan partial while
    // saying nothing about the artifact tree beside it. Both ignored paths are
    // committed to .gitignore so each tree is quiet, leaving per-tree
    // completeness as the only thing that could separate the two.
    for name in ["complete", "unread"] {
        let r = repo(&root, name);
        fs::write(r.join(".gitignore"), "__pycache__/\n.unreadable/\n").unwrap();
        fs::write(r.join("source.py"), "print('preserve')").unwrap();
        crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
        crate::engine::git_cli::git_text(
            &r,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        )
        .unwrap();
        fs::create_dir(r.join("__pycache__")).unwrap();
        fs::write(r.join("__pycache__/module.pyc"), vec![b'x'; 100]).unwrap();
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
        for p in [r.join("__pycache__/module.pyc"), r.join("__pycache__")] {
            fs::File::open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
    }
    let blocked = root.join("unread/.unreadable");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("entry"), "unreadable").unwrap();
    let denied = deny(&blocked);

    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "mixed-run".into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let mut gated: Vec<String> = vec![];
    let outcome = cleaner.perform_with_gate(&saved.config, &mut run, |repo, _, _| {
        gated.push(repo.to_string());
        Ok(())
    });
    // Restored before the assertions so they see the tree as a user would;
    // `Denied` still restores it if anything above this line panics.
    denied.restore();
    outcome.expect("one unreadable directory must not fail the whole run");

    assert!(
        run.partial,
        "part of the workspace was not read and the run must still say so"
    );
    let mut gated_names: Vec<&str> = gated
        .iter()
        .map(|g| g.rsplit('/').next().unwrap_or(g))
        .collect();
    gated_names.sort_unstable();
    assert_eq!(
        gated_names,
        ["complete", "unread"],
        "both artifact trees were enumerated to the end, so both are gated \
         for removal; the unreadable sibling disqualifies neither: {gated:?}"
    );
    assert_eq!(
        run.items.len(),
        2,
        "both artifacts are attempted: {:?}",
        run.items
    );
    assert!(
        run.items.iter().all(|i| i.status == "completed"),
        "{:?}",
        run.items
    );
    assert_eq!(
        run.status, "completed_with_skips",
        "a run that could not read part of the workspace claims no complete \
         coverage, even when every artifact it did examine was removed"
    );
    assert!(
        run.issues
            .iter()
            .any(|i| i.contains("Partial repository scan") && i.ends_with("/unread")),
        "the directory that could not be read must still be reported: {:?}",
        run.issues
    );
    assert!(
        !root.join("complete/__pycache__").exists(),
        "the stale cache in the fully readable repository should have been removed"
    );
    assert!(
        !root.join("unread/__pycache__").exists(),
        "a stale, ignored, untracked cache that WAS read to the end is \
         removable even though a sibling directory in the same repository \
         could not be read"
    );
    assert!(
        root.join("unread/.unreadable").is_dir(),
        "the directory that could not be read is left exactly as it was"
    );
    assert!(root.join("complete/source.py").is_file());
    assert!(root.join("unread/source.py").is_file());
}

/// The other half of the narrower veto, and the half that must not regress:
/// move the unreadable directory from beside the artifact to inside it, and the
/// artifact stops being removable.
///
/// This is what makes the test above a narrowing rather than a weakening. Here
/// the walk really cannot say what the tree holds, so its byte figure is a
/// floor and its contents are unknown; `walk_worktree` gives the tree its own
/// `artifact_child()` budget, the denial sets `truncated` on that budget, and
/// `scan_complete` is therefore false. The fixture is the one above with a
/// single line moved, so per-tree completeness is the only difference between
/// the two outcomes.
///
/// Two independent layers refuse this tree, which is good for the owner's files
/// and awkward for a test: most of what is asserted below would hold even if
/// the layer under test were gone. So the assertions are labelled rather than
/// left to look equally load-bearing.
///
/// The layer under test is `inspect`'s `scan_complete` filter, which never
/// offers the tree as a candidate. Forcing `complete = true` in `walk_worktree`
/// discriminates on exactly two assertions: `run.items.len() == 1`, measured
/// red (`left: 2, right: 1`), and the "Artifact tree not read in full … /unread"
/// issue line, which sits behind that panic so it was not reached -- red by
/// mechanism, since with nothing reporting `!scan_complete` there is no
/// artifact for `inspect` to derive the line from.
///
/// Everything else stays green under that revert and is therefore not evidence
/// about `scan_complete`. Measured green: both bytes-survive assertions,
/// `complete/__pycache__` removed, and `gated.len() == 1` -- the gate runs only
/// after `prepare` succeeds, and here `tree::snapshot` in strict mode refuses
/// first with "Python bytecode directory contains non-bytecode content",
/// rejecting the subdirectory as content before anything tries to read it (for
/// a provider without that content rule it would be the `read_dir` denial
/// instead). Green by mechanism, not reached because the panic preceded them:
/// `run.partial` and `completed_with_skips`, both of which survive because a
/// denial under the artifact root still reaches `scan.permission_denied`
/// through `absorb_artifact` whatever `complete` says.
///
/// Stated rather than assumed, because a test that cannot fail proves nothing.
#[cfg(unix)]
#[test]
fn an_unreadable_directory_inside_an_artifact_tree_keeps_it_ineligible() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("inside-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);

    for name in ["complete", "unread"] {
        let r = repo(&root, name);
        fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
        fs::write(r.join("source.py"), "print('preserve')").unwrap();
        crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
        crate::engine::git_cli::git_text(
            &r,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-qm",
                "fixture",
            ],
        )
        .unwrap();
        fs::create_dir(r.join("__pycache__")).unwrap();
        fs::write(r.join("__pycache__/module.pyc"), vec![b'x'; 100]).unwrap();
        // The one difference from the sibling case: this subdirectory is under
        // the artifact root, so the denial lands on the artifact's own budget
        // rather than on the repository's.
        if name == "unread" {
            let blocked = r.join("__pycache__/blocked");
            fs::create_dir(&blocked).unwrap();
            fs::write(blocked.join("entry"), vec![b'x'; 100]).unwrap();
        }
        // Stamped after that subdirectory exists, because creating it refreshes
        // `__pycache__`'s own mtime. Nothing inside `blocked` is stamped:
        // `require_age` runs inside `prepare_with_activity`, downstream of
        // `inspect`'s completeness filter, so it could not keep this tree from
        // reaching that filter however fresh it was -- and here it is never
        // reached at all, because the strict snapshot rejects `blocked` as
        // non-bytecode content before it reads a stamp.
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
        for p in [r.join("__pycache__/module.pyc"), r.join("__pycache__")] {
            fs::File::open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
    }
    let blocked = root.join("unread/__pycache__/blocked");
    let denied = deny(&blocked);

    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "inside-run".into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let mut gated: Vec<String> = vec![];
    let outcome = cleaner.perform_with_gate(&saved.config, &mut run, |repo, _, _| {
        gated.push(repo.to_string());
        Ok(())
    });
    // Restored before the assertions, which read through this directory to
    // prove its contents survived; `Denied` covers the panic path above.
    denied.restore();
    outcome.expect("one half-read tree must not fail the whole run");

    assert!(
        root.join("unread/__pycache__/module.pyc").is_file(),
        "a tree the walk could not finish must not be removed"
    );
    assert!(
        root.join("unread/__pycache__/blocked/entry").is_file(),
        "nor anything inside the part it could not read"
    );
    assert!(
        !root.join("complete/__pycache__").exists(),
        "the tree that WAS read to the end is still removed: {:?}",
        run.items
    );
    assert_eq!(
        gated.len(),
        1,
        "only the fully enumerated tree reaches the removal gate: {gated:?}"
    );
    assert_eq!(
        run.items.len(),
        1,
        "the half-read tree is never offered as a candidate, so nothing is \
         attempted for it: {:?}",
        run.items
    );
    assert!(
        run.items[0].repo_path.ends_with("/complete"),
        "{:?}",
        run.items
    );
    assert!(
        run.partial,
        "a tree that could not be read in full makes the run partial"
    );
    assert!(
        run.issues
            .iter()
            .any(|i| i.contains("Artifact tree not read in full") && i.ends_with("/unread")),
        "the half-read tree must be named, not silently dropped: {:?}",
        run.issues
    );
    assert_eq!(
        run.status, "completed_with_skips",
        "a run holding a tree it could not read claims no complete coverage"
    );
    assert!(root.join("unread/source.py").is_file());
}

/// The reconciliation, end to end, in the shape that motivated it.
///
/// DevCouncil's repository scan is truncated for reasons that belong to the
/// repository and to no tree inside it -- 17 linked worktrees against a sizing
/// cap of 8, and more artifact directories than the reporting cap -- while its
/// `rust/target` was enumerated to the end. Keying `incomplete_repos` on
/// `scan.truncated` put the repository in that set, `eligible_candidates`
/// dropped every one of its candidates, and `candidates.is_empty() &&
/// run.partial` then turned the whole run into an error. 64 GiB that the scan
/// had read in full, refused by a flag about linked worktrees.
///
/// This fixture uses the artifact reporting cap because it needs no git calls:
/// 65 unignored artifact directories push the list past `MAX_ARTIFACT_DIRS` and
/// set `scan.truncated`, and being unignored they are not candidates
/// themselves. The one stale, ignored, fully-read tree is.
#[cfg(unix)]
#[test]
fn a_truncated_repository_scan_still_cleans_a_tree_that_was_read_in_full() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("truncated-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);

    let r = repo(&root, "big");
    fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
    fs::write(r.join("source.py"), "print('preserve')").unwrap();
    crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();

    // The tree to be reclaimed: ignored, long stale, read to the end, and the
    // largest artifact so the reporting cap cannot drop it.
    fs::create_dir(r.join("__pycache__")).unwrap();
    fs::write(r.join("__pycache__/module.pyc"), vec![b'x'; 4096]).unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
    for p in [r.join("__pycache__/module.pyc"), r.join("__pycache__")] {
        fs::File::open(p)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(old))
            .unwrap();
    }

    // Enough artifact directories to push the report past its reporting cap,
    // which is what makes `scan.truncated` true. They are COMMITTED, for two
    // reasons: tracked files make each one ineligible on its own merits, so the
    // tree above is the only candidate; and `quiet_repo` refuses a repository
    // with any `git status` output at all, so leaving them untracked made the
    // repository dirty and nothing was gated -- the fixture defeating its own
    // test. They still count towards the reporting cap, which is applied before
    // any git filtering.
    for i in 0..=64 {
        let filler = r.join(format!("f{i}/coverage"));
        fs::create_dir_all(&filler).unwrap();
        fs::write(filler.join("report.txt"), b"x").unwrap();
    }
    crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fillers",
        ],
    )
    .unwrap();

    cleaner.save(config(&cleaner, &root)).unwrap();
    let inventory = cleaner.inventory().expect("inventory");
    assert!(
        inventory.partial,
        "the repository scan really is truncated, and the run must keep saying so"
    );
    assert!(
        inventory.incomplete_repos.is_empty(),
        "a truncated scan is not a repository that could not be read: {:?}",
        inventory.incomplete_repos
    );
    let paths: Vec<&str> = inventory
        .candidates
        .iter()
        .map(|c| c.path.as_str())
        .collect();
    assert_eq!(
        paths,
        vec!["__pycache__"],
        "the tree that was read in full is the one candidate: {paths:?}"
    );

    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "truncated-run".into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let mut gated: Vec<String> = vec![];
    cleaner
        .perform_with_gate(&saved.config, &mut run, |repo, _, _| {
            gated.push(repo.to_string());
            Ok(())
        })
        .expect("a truncated repository scan is not a reason to fail the run");

    assert_eq!(gated.len(), 1, "the readable tree is gated: {gated:?}");
    assert_eq!(
        run.items.len(),
        1,
        "and it is the only thing attempted: {:?}",
        run.items
    );
    assert_eq!(run.items[0].status, "completed", "{:?}", run.items);
    assert!(
        run.items[0].path.ends_with("__pycache__"),
        "{:?}",
        run.items
    );
    assert!(
        run.partial,
        "the run still claims no complete coverage of the workspace"
    );
    assert!(
        !r.join("__pycache__").exists(),
        "the tree that was read in full was actually removed"
    );
    assert!(
        r.join("source.py").is_file(),
        "and nothing else was touched"
    );
}

/// The other side of making eligibility per tree: if the only trees in the
/// workspace were not read in full, nothing is eligible, and that has to be a
/// refusal rather than a clean run of zero items. The refusal is guarded by
/// `candidates.is_empty() && run.partial`, so this is really a test that
/// per-tree incompleteness reaches `partial` -- which `inspect` derives from
/// `scan_complete` directly rather than inferring from `scan.truncated`.
#[cfg(unix)]
#[test]
fn a_workspace_whose_every_tree_was_cut_short_refuses_instead_of_reporting_success() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("all-unread-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);

    let r = repo(&root, "only");
    fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
    fs::create_dir(r.join("__pycache__")).unwrap();
    fs::write(r.join("__pycache__/module.pyc"), vec![b'x'; 100]).unwrap();
    // A subtree of the artifact itself that the walk cannot enter, so the only
    // candidate tree in the workspace is the one that was not read in full.
    let blocked = r.join("__pycache__/inner");
    fs::create_dir(&blocked).unwrap();
    fs::write(blocked.join("entry"), "unreadable").unwrap();
    let denied = deny(&blocked);

    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = CleanerRun {
        id: "all-unread-run".into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let mut gated: Vec<String> = vec![];
    let outcome = cleaner.perform_with_gate(&saved.config, &mut run, |repo, _, _| {
        gated.push(repo.to_string());
        Ok(())
    });
    denied.restore();

    assert!(
        outcome.is_err(),
        "nothing was eligible and something could not be read: that is a refusal"
    );
    assert_eq!(run.status, "incomplete");
    assert!(gated.is_empty(), "nothing may be gated: {gated:?}");
    assert!(run.items.is_empty(), "{:?}", run.items);
    assert!(
        r.join("__pycache__/module.pyc").is_file(),
        "and the tree survives on disk"
    );
}

/// A committed repository holding one aged, ignored `__pycache__` per entry of
/// `dirs` ("" for the repository root), each `bytes` long.
#[cfg(unix)]
fn aged_bytecode_repo(
    root: &std::path::Path,
    name: &str,
    dirs: &[&str],
    bytes: usize,
) -> std::path::PathBuf {
    let r = repo(root, name);
    fs::write(r.join(".gitignore"), "__pycache__/\n").unwrap();
    fs::write(r.join("source.py"), "print('preserve')").unwrap();
    crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ],
    )
    .unwrap();
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
    for dir in dirs {
        let cache = r.join(dir).join("__pycache__");
        fs::create_dir_all(&cache).unwrap();
        let file = cache.join("module.pyc");
        fs::write(&file, vec![b'x'; bytes]).unwrap();
        for p in [file, cache] {
            fs::File::open(p)
                .unwrap()
                .set_times(fs::FileTimes::new().set_modified(old))
                .unwrap();
        }
    }
    r
}

/// A journaled, running history entry for `perform_with_gate` to fill.
fn journaled_run(cleaner: &Cleaner, id: &str) -> CleanerRun {
    let run = CleanerRun {
        id: id.into(),
        trigger: "manual".into(),
        started_at: crate::storage::hygiene::now(),
        finished_at: 0,
        status: "running".into(),
        repositories: 0,
        partial: false,
        issues: vec![],
        items: vec![],
        unlisted: 0,
        unexamined: 0,
    };
    cleaner
        .update(|s| {
            s.history.push(run.clone());
            Ok(())
        })
        .unwrap();
    run
}

/// Measured 2026-11-14: twenty consecutive scheduled runs reclaimed nothing,
/// each one filled to `max_targets` with refused rows. A refused candidate is
/// not an attempted removal, so it must not use up the run's target count.
#[cfg(unix)]
#[test]
fn refused_candidates_do_not_use_up_the_target_count() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("refusals-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    // Larger trees sort first, so the two dirty repositories are reached
    // before the clean one.
    for name in ["dirty-a", "dirty-b"] {
        let r = aged_bytecode_repo(&root, name, &[""], 4096);
        fs::write(r.join("source.py"), "print('edited')").unwrap();
    }
    let clean = aged_bytecode_repo(&root, "clean", &[""], 100);
    let mut c = config(&cleaner, &root);
    c.max_targets = 1;
    let saved = cleaner.save(c).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "refusals-run");
    let _run_lock = cleaner.lock("run.lock").unwrap();
    cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| Ok(()))
        .unwrap();

    assert!(
        !clean.join("__pycache__").exists(),
        "the clean repository's output is removed: {run:?}"
    );
    for name in ["dirty-a", "dirty-b"] {
        assert!(root.join(name).join("__pycache__/module.pyc").is_file());
    }
    assert_eq!(
        run.items.iter().filter(|r| r.status == "completed").count(),
        1,
        "{run:?}"
    );
    assert_eq!(
        run.items
            .iter()
            .filter(|r| r.message.contains("uncommitted work"))
            .count(),
        2,
        "{run:?}"
    );
}

/// Once refused rows stop counting against `max_targets`, one run can refuse
/// hundreds of candidates. `read` refuses a history holding a run of more than
/// `MAX_RUN_ITEMS` rows, so an unbounded list would make every later run fail.
#[cfg(unix)]
#[test]
fn a_run_of_many_refusals_keeps_its_history_readable_and_counts_what_it_did_not_list() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("many-refusals-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    service.quiet_override = Some(|_| Err("Repository has active task leases".into()));
    let cleaner = Arc::new(service);
    let dirs: Vec<String> = (0..50).map(|i| format!("pkg{i:02}")).collect();
    let dirs: Vec<&str> = dirs.iter().map(String::as_str).collect();
    for name in ["r1", "r2", "r3"] {
        aged_bytecode_repo(&root, name, &dirs, 100);
    }
    let mut c = config(&cleaner, &root);
    c.max_targets = 20;
    let saved = cleaner.save(c).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "many-refusals-run");
    let _run_lock = cleaner.lock("run.lock").unwrap();
    cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| {
            panic!("a refused repository is never gated")
        })
        .unwrap();

    assert_eq!(run.items.len(), 80, "room is kept for 20 attempts");
    assert_eq!(run.unlisted, 70, "150 refusals, 80 listed");
    assert!(run.items.iter().all(|r| r.status == "skipped"));
    assert!(
        run.issues.iter().any(|i| i.contains("70 more")),
        "{:?}",
        run.issues
    );
    cleaner.record(&run).unwrap();
    cleaner
        .read()
        .expect("the saved history stays readable after the run");
}

/// A check the host-load governor refused, typed as the spawn gate types it.
fn shed() -> crate::ipc_error::CommandError {
    crate::ipc_error::CommandError::typed(
        crate::ipc_error::IpcErrorKind::DeferredUnderLoad,
        "git status deferred under load after 0.000s: reactive work is shed while the host is under memory pressure",
    )
}

/// Measured 2026-11-14: 14 of 20 rows read "skipped ... deferred under load".
/// A check that never ran is not a refusal, and must not read like one.
#[cfg(unix)]
#[test]
fn a_check_the_host_shed_is_reported_unexamined_not_skipped() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("shed-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    service.quiet_override = Some(|repo| {
        if repo.ends_with("/loaded") {
            Err(shed())
        } else {
            super::quiet_repo(repo)
        }
    });
    let cleaner = Arc::new(service);
    let loaded = aged_bytecode_repo(&root, "loaded", &[""], 4096);
    let calm = aged_bytecode_repo(&root, "calm", &[""], 100);
    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "shed-run");
    let _run_lock = cleaner.lock("run.lock").unwrap();
    cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| Ok(()))
        .unwrap();

    let row = run
        .items
        .iter()
        .find(|r| r.repo_path.ends_with("/loaded"))
        .expect("the shed candidate has a row");
    assert_eq!(row.status, "unexamined", "{row:?}");
    assert!(row.message.starts_with("Could not examine"), "{row:?}");
    assert_eq!(run.unexamined, 1);
    assert!(loaded.join("__pycache__/module.pyc").is_file());
    assert!(!calm.join("__pycache__").exists(), "{run:?}");
}

static SHED_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// A host that sheds a check briefly is asked again within the same run
/// instead of leaving the tree for a whole interval.
#[cfg(unix)]
#[test]
fn a_briefly_shed_check_is_asked_again_within_the_run() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("brief-shed-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    SHED_CALLS.store(0, std::sync::atomic::Ordering::SeqCst);
    service.quiet_override = Some(|repo| {
        if SHED_CALLS.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Err(shed())
        } else {
            super::quiet_repo(repo)
        }
    });
    let cleaner = Arc::new(service);
    let r = aged_bytecode_repo(&root, "only", &[""], 100);
    let saved = cleaner.save(config(&cleaner, &root)).unwrap();
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "brief-shed-run");
    let _run_lock = cleaner.lock("run.lock").unwrap();
    cleaner
        .perform_with_gate(&saved.config, &mut run, |_, _, _| Ok(()))
        .unwrap();
    assert!(!r.join("__pycache__").exists(), "{run:?}");
    assert_eq!(run.unexamined, 0, "{run:?}");
    assert!(SHED_CALLS.load(std::sync::atomic::Ordering::SeqCst) >= 3);
}

/// A shed run brings an enabled schedule forward, backing off, and gives up
/// after `MAX_DEFERRAL_RETRIES` in a row; a run that examined everything
/// resets the count.
#[cfg(unix)]
#[test]
fn a_shed_run_brings_the_schedule_forward_with_a_bounded_backoff() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, root) = fixture();
    let now = crate::storage::hygiene::now();
    let mut c = config(&cleaner, &root);
    c.enabled = true;
    c.interval_hours = 24;
    c.next_run_at = now + 86_400;
    let saved = cleaner.save_with_background(c, |_| Ok(())).unwrap();
    let ran_with = saved.config;
    let mut run = journaled_run(&cleaner, "shed-schedule-run");
    run.unexamined = 3;
    for step in 0..super::MAX_DEFERRAL_RETRIES {
        // Each attempt is judged against the policy it ran with.
        let current = cleaner.state().unwrap().config;
        let mut ran = ran_with.clone();
        ran.revision = current.revision;
        let moved = cleaner
            .retry_sooner_after_shedding(&ran, &run, now)
            .unwrap();
        let delay = super::DEFERRAL_RETRY_FIRST_SECS << step;
        assert_eq!(
            moved,
            Some(now + delay).filter(|at| *at < current.next_run_at)
        );
    }
    let current = cleaner.state().unwrap().config;
    assert_eq!(current.next_run_at, now + super::DEFERRAL_RETRY_FIRST_SECS);
    let mut ran = ran_with.clone();
    ran.revision = current.revision;
    assert_eq!(
        cleaner
            .retry_sooner_after_shedding(&ran, &run, now)
            .unwrap(),
        None,
        "a host that stays loaded falls back to the interval"
    );
    assert_eq!(
        cleaner.read().unwrap().deferral_retries,
        super::MAX_DEFERRAL_RETRIES
    );
    run.unexamined = 0;
    cleaner
        .retry_sooner_after_shedding(&ran, &run, now)
        .unwrap();
    assert_eq!(cleaner.read().unwrap().deferral_retries, 0);
    // A policy edited since the run is never moved by it.
    run.unexamined = 1;
    ran.revision = ran.revision.wrapping_sub(1);
    assert_eq!(
        cleaner
            .retry_sooner_after_shedding(&ran, &run, now)
            .unwrap(),
        None
    );
}

#[test]
fn every_reclaim_scope_is_off_by_default_and_absent_from_saved_state() {
    let config = CleanerConfig::default();
    assert!(!config.scopes.any());
    assert_eq!(config.scopes.worktree_idle_days, 7);
    assert_eq!(
        config.clone_budget.max_target_clones,
        super::DEFAULT_MAX_TARGET_CLONES
    );
    let json = serde_json::to_string(&Saved::default()).unwrap();
    for field in [
        "scopes",
        "clone_budget",
        "previewed_scopes",
        "deferral_retries",
    ] {
        assert!(
            !json.contains(field),
            "a default {field} must not be written, so an older build can read the file: {json}"
        );
    }
    let mut short = CleanerConfig::default();
    short.scopes.worktree_idle_days = 3;
    assert!(
        short.validate().is_err(),
        "the worktree scopes keep DevCouncil's scheduled 7-day minimum"
    );
}

fn set_old(path: &std::path::Path) {
    let old = std::time::SystemTime::now() - std::time::Duration::from_secs(100 * 86400);
    fs::File::open(path)
        .unwrap()
        .set_times(fs::FileTimes::new().set_modified(old))
        .unwrap();
}

/// A Cargo repository on `main` with one linked worktree under
/// `.claude/worktrees/` (a dot directory discovery never enters, as the
/// measured agent worktrees are) whose `target/` is an aged, ignored Cargo
/// build directory. Its payload is cloned from a file outside the repository
/// where the volume supports clones, so its logical size overstates what
/// removing it frees.
#[cfg(unix)]
fn worktree_with_cargo_target(root: &std::path::Path) -> (std::path::PathBuf, std::path::PathBuf) {
    let r = repo(root, "cargo-repo");
    crate::engine::git_cli::git_text(&r, &["checkout", "-q", "-b", "main"]).unwrap();
    fs::write(r.join(".gitignore"), "target/\n.claude/\n").unwrap();
    fs::write(r.join("Cargo.toml"), "[package]\nname = \"fixture\"\n").unwrap();
    crate::engine::git_cli::git_text(&r, &["add", "."]).unwrap();
    // Committed long ago, so the worktree made from it is idle, not unstarted.
    let status = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "-qm",
            "fixture",
        ])
        .current_dir(&r)
        .env("GIT_AUTHOR_DATE", "2020-01-01T00:00:00Z")
        .env("GIT_COMMITTER_DATE", "2020-01-01T00:00:00Z")
        .status()
        .unwrap();
    assert!(status.success());
    let wt = r.join(".claude/worktrees/agent-x");
    fs::create_dir_all(wt.parent().unwrap()).unwrap();
    crate::engine::git_cli::git_text(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-x",
            wt.to_str().unwrap(),
        ],
    )
    .unwrap();
    // Trust is per repository: a linked worktree is granted on its own.
    crate::test_support::trust_repo(&wt);
    set_old(&r.join(".git/worktrees/agent-x/index"));
    let target = wt.join("target");
    fs::create_dir_all(target.join("debug")).unwrap();
    fs::write(
        target.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::write(target.join(".rustc_info.json"), "{}").unwrap();
    let anchor = root.join("anchor.bin");
    let payload: Vec<u8> = (0..4 * 1024 * 1024usize)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8 | 1)
        .collect();
    fs::write(&anchor, &payload).unwrap();
    fs::File::open(&anchor).unwrap().sync_all().unwrap();
    let blob = target.join("debug/blob.bin");
    #[cfg(target_os = "macos")]
    {
        let src = std::ffi::CString::new(anchor.to_str().unwrap()).unwrap();
        let dst = std::ffi::CString::new(blob.to_str().unwrap()).unwrap();
        // SAFETY: both are valid NUL-terminated paths.
        assert_eq!(unsafe { libc::clonefile(src.as_ptr(), dst.as_ptr(), 0) }, 0);
    }
    #[cfg(not(target_os = "macos"))]
    fs::copy(&anchor, &blob).unwrap();
    for p in [
        target.join("debug/blob.bin"),
        target.join("CACHEDIR.TAG"),
        target.join(".rustc_info.json"),
        target.join("debug"),
        target.clone(),
    ] {
        set_old(&p);
    }
    (r, wt)
}

fn scoped(
    cleaner: &Arc<Cleaner>,
    root: &std::path::Path,
    scopes: super::ReclaimScopes,
) -> CleanerConfig {
    let mut c = config(cleaner, root);
    c.scopes = scopes;
    cleaner.save(c).unwrap().config
}

fn run_now(cleaner: &Arc<Cleaner>, config: &CleanerConfig, id: &str) -> CleanerRun {
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(cleaner, id);
    let _run_lock = cleaner.lock("run.lock").unwrap();
    let outcome = cleaner.perform_with_gate(config, &mut run, |_, _, _| Ok(()));
    assert!(outcome.is_ok(), "{outcome:?}: {:?}", run.issues);
    run
}

/// The worktree-cache scope reaches a linked worktree's cloned `target/`,
/// budgets it by what removing it frees, and is used by no run until a person
/// has previewed the enabled set; changing the set needs a fresh preview.
#[cfg(unix)]
#[test]
fn a_worktree_cache_is_reclaimed_only_after_its_scope_was_previewed() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("scope-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    let (_repo, wt) = worktree_with_cargo_target(&root);
    let scopes = super::ReclaimScopes {
        worktree_caches: true,
        ..Default::default()
    };
    let mut c = config(&cleaner, &root);
    c.scopes = scopes;
    // Logical 4 MiB, cloned: it fits a 1 MiB budget only by private bytes.
    c.max_run_bytes = if cfg!(target_os = "macos") {
        1024 * 1024
    } else {
        64 * 1024 * 1024
    };
    let saved = cleaner.save(c).unwrap().config;
    assert!(!cleaner.state().unwrap().scopes_previewed);

    // Without the scope nothing here is eligible. The main checkout's walk
    // reads as partial with a linked worktree inside it (an existing
    // property of `walk_worktree`), so the run may end in the ordinary
    // "nothing eligible and something unread" refusal; either way nothing in
    // the worktree is touched.
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "unpreviewed-run");
    {
        let _run_lock = cleaner.lock("run.lock").unwrap();
        let outcome = cleaner.perform_with_gate(&saved, &mut run, |_, _, _| {
            panic!("an unpreviewed scope offers nothing to gate")
        });
        assert!(
            outcome
                .as_ref()
                .map_or_else(|e| e.message().contains("Incomplete inventory"), |()| true),
            "{outcome:?}"
        );
    }
    assert!(wt.join("target/debug/blob.bin").is_file(), "{run:?}");
    assert!(
        run.issues
            .iter()
            .any(|i| i.contains("have not been previewed")),
        "{:?}",
        run.issues
    );

    let preview = cleaner.inventory().unwrap();
    let candidate = preview
        .candidates
        .iter()
        .find(|c| c.scope == "worktree-cache")
        .unwrap_or_else(|| panic!("the preview names the worktree cache: {preview:?}"));
    assert_eq!(candidate.path, "target");
    #[cfg(target_os = "macos")]
    assert!(
        candidate.private_bytes.unwrap() < candidate.bytes / 4,
        "{candidate:?}"
    );
    assert!(cleaner.state().unwrap().scopes_previewed);

    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "previewed-run");
    assert!(!wt.join("target").exists(), "{run:?}");
    assert!(wt.join("Cargo.toml").is_file(), "the worktree itself stays");

    // A changed scope set is unseen again.
    let mut c = cleaner.state().unwrap().config;
    c.scopes.prune_worktrees = true;
    cleaner.save(c).unwrap();
    assert!(!cleaner.state().unwrap().scopes_previewed);
}

/// The task store is per checkout. A lane's lease may live in the main
/// checkout's store (naming the lane's branch) or in the lane's own; the
/// worktree scopes read both. Before, only the worktree's own store was read,
/// so a lease the main checkout held for the lane's branch was never seen.
#[cfg(unix)]
#[test]
fn a_lease_on_the_worktrees_branch_in_either_task_store_holds_its_cache() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("lease-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    let (repo, wt) = worktree_with_cargo_target(&root);
    fs::write(repo.join(".git/info/exclude"), ".devcouncil/\n").unwrap();
    let lease = |checkout: &std::path::Path, id: &str, branch: Option<&str>| {
        let db = crate::tasks::store_path(checkout.to_str().unwrap());
        fs::create_dir_all(db.parent().unwrap()).unwrap();
        drop(dc_store::Store::open(&db).unwrap());
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute(
                "INSERT INTO task_leases (id, task_id, owner, lease_token, status, branch, created_at, expires_at)
                 VALUES (?1, ?1, 'agent', 'token', 'active', ?2, '2026-01-01T00:00:00Z', '2999-01-01T00:00:00Z')",
                rusqlite::params![id, branch],
            )
            .unwrap();
        db
    };
    let scopes = super::ReclaimScopes {
        worktree_caches: true,
        ..Default::default()
    };
    scoped(&cleaner, &root, scopes);

    // The main checkout's store leases the lane's branch.
    let main_store = lease(&repo, "main-held", Some("agent-x"));
    cleaner.inventory().unwrap();
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "main-lease-run");
    assert!(wt.join("target/debug/blob.bin").is_file(), "{run:?}");
    assert!(
        run.items
            .iter()
            .any(|item| item.message.contains("active task leases")),
        "{run:?}"
    );
    fs::remove_dir_all(main_store.parent().unwrap()).unwrap();

    // The lane's own store holds a lease, whatever branch it names.
    lease(&wt, "own-held", Some("some-other-branch"));
    cleaner.inventory().unwrap();
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "own-lease-run");
    assert!(wt.join("target/debug/blob.bin").is_file(), "{run:?}");
}

/// The worktree scopes judge the worktree, not the repository: uncommitted
/// work in the main checkout does not hold a clean worktree's cache, and
/// uncommitted work in the worktree does.
#[cfg(unix)]
#[test]
fn the_worktree_scope_vetoes_on_the_worktree_not_the_main_checkout() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("veto-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    let (repo, wt) = worktree_with_cargo_target(&root);
    fs::write(repo.join("Cargo.toml"), "edited in the main checkout").unwrap();
    fs::write(wt.join("Cargo.toml"), "edited in the worktree").unwrap();
    scoped(
        &cleaner,
        &root,
        super::ReclaimScopes {
            worktree_caches: true,
            ..Default::default()
        },
    );
    cleaner.inventory().unwrap();
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "dirty-worktree-run");
    assert!(wt.join("target/debug/blob.bin").is_file(), "{run:?}");
    assert!(run
        .items
        .iter()
        .any(|r| r.message.contains("Worktree has uncommitted work")));

    crate::engine::git_cli::git_text(&wt, &["checkout", "--", "Cargo.toml"]).unwrap();
    let run = run_now(&cleaner, &saved, "clean-worktree-run");
    assert!(
        !wt.join("target").exists(),
        "the main checkout's edit does not hold it: {run:?}"
    );
}

/// `per_tree_vetoes` refuses output only for uncommitted work in the project
/// that produced it; off, any uncommitted work in the repository refuses.
#[cfg(unix)]
#[test]
fn per_tree_vetoes_judge_the_producing_project_only() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("per-tree-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    let r = aged_bytecode_repo(&root, "mono", &["quiet", "busy"], 100);
    fs::create_dir_all(r.join("elsewhere")).unwrap();
    fs::write(r.join("elsewhere/notes.md"), "untracked work").unwrap();
    fs::write(r.join("busy/edit.py"), "untracked work in the project").unwrap();

    let saved = scoped(&cleaner, &root, super::ReclaimScopes::default());
    let run = run_now(&cleaner, &saved, "repo-veto-run");
    assert!(r.join("quiet/__pycache__").exists(), "{run:?}");

    scoped(
        &cleaner,
        &root,
        super::ReclaimScopes {
            per_tree_vetoes: true,
            ..Default::default()
        },
    );
    cleaner.inventory().unwrap();
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "tree-veto-run");
    assert!(!r.join("quiet/__pycache__").exists(), "{run:?}");
    assert!(
        r.join("busy/__pycache__/module.pyc").is_file(),
        "its own project has uncommitted work: {run:?}"
    );
}

/// The prune scope removes an integrated, clean, unused linked worktree with
/// an unforced `git worktree remove`, and leaves a dirty one.
#[cfg(unix)]
#[test]
fn the_prune_scope_removes_only_a_stale_worktree_and_never_forces() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("prune-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    let cleaner = Arc::new(service);
    let (repo, stale) = worktree_with_cargo_target(&root);
    let dirty = repo.join(".claude/worktrees/agent-y");
    crate::engine::git_cli::git_text(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "agent-y",
            dirty.to_str().unwrap(),
        ],
    )
    .unwrap();
    crate::test_support::trust_repo(&dirty);
    fs::write(dirty.join("Cargo.toml"), "uncommitted").unwrap();
    scoped(
        &cleaner,
        &root,
        super::ReclaimScopes {
            prune_worktrees: true,
            ..Default::default()
        },
    );
    let preview = cleaner.inventory().unwrap();
    assert_eq!(
        preview
            .candidates
            .iter()
            .filter(|c| c.scope == "prune-worktree")
            .count(),
        1,
        "{preview:?}"
    );
    let saved = cleaner.state().unwrap().config;
    let mut gated = vec![];
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut run = journaled_run(&cleaner, "prune-run");
    {
        let _run_lock = cleaner.lock("run.lock").unwrap();
        cleaner
            .perform_with_gate(&saved, &mut run, |_, argv, _| {
                gated.push(argv.map(<[String]>::to_vec));
                Ok(())
            })
            .unwrap();
    }
    assert!(!stale.exists(), "{run:?}");
    assert!(dirty.join("Cargo.toml").is_file());
    let argv = gated
        .iter()
        .flatten()
        .find(|argv| argv.iter().any(|a| a == "remove"))
        .expect("the removal was gated with its argv");
    assert!(
        !argv.iter().any(|a| a == "--force" || a == "-f"),
        "{argv:?}"
    );
}

/// Read-only: what the reclaim scopes WOULD offer in a real repository, with
/// private bytes. Runs only when `GITPULSE_DRY_RUN_ROOT` names a directory,
/// under the test process's hermetic HOME (its trust grants and cleaner state
/// are temporary, never the owner's), with a temporary state directory instead
/// of `service()`. It inventories and measures; it never prepares, gates or
/// removes anything. Prints one tab-separated row per candidate and per
/// linked worktree.
#[test]
#[ignore = "reads a real repository; run by hand with GITPULSE_DRY_RUN_ROOT"]
fn dry_run_reclaim_table() {
    let Some(root) = std::env::var_os("GITPULSE_DRY_RUN_ROOT") else {
        return;
    };
    let root = crate::engine::git_cli::canonicalize_plain(std::path::Path::new(&root)).unwrap();
    crate::test_support::trust_repo(&root);
    let infos = crate::engine::worktree::list_worktrees_lite(root.to_str().unwrap()).unwrap();
    for info in infos.iter().filter(|i| !i.is_main) {
        crate::test_support::trust_repo(std::path::Path::new(&info.path));
    }
    let state = tempfile::tempdir().unwrap();
    let cleaner = Cleaner::new(state.path().join("hygiene"));
    let mut config = CleanerConfig {
        roots: vec![root.to_string_lossy().into_owned()],
        ..CleanerConfig::default()
    };
    config.scopes = super::ReclaimScopes {
        worktree_caches: true,
        per_tree_vetoes: false,
        prune_worktrees: true,
        worktree_idle_days: 7,
    };
    let inventory = cleaner.inspect(&config, config.scopes, true).unwrap();
    println!("DRY\tscope\trepo\tpath\tlogical\tprivate\tage_gate");
    for c in &inventory.candidates {
        let target = if c.scope == "prune-worktree" {
            std::path::PathBuf::from(&c.path)
        } else {
            std::path::Path::new(&c.repo_path).join(&c.path)
        };
        let age = if c.scope == "prune-worktree" {
            "n/a".to_string()
        } else {
            let days = if c.scope == "worktree-cache" {
                config.scopes.worktree_idle_days
            } else {
                config.retention_days
            };
            match crate::storage::hygiene::tree::snapshot(&target, true, &AtomicBool::new(false)) {
                Ok(snap) => match crate::storage::hygiene::tree::require_age(
                    &snap,
                    days,
                    std::time::SystemTime::now(),
                ) {
                    Ok(()) => format!("passes {days}d"),
                    Err(_) => format!("newer than {days}d"),
                },
                Err(e) => format!("unread: {e}"),
            }
        };
        println!(
            "DRY\t{}\t{}\t{}\t{}\t{}\t{}",
            c.scope,
            c.repo_path,
            c.path,
            c.bytes,
            c.private_bytes.map_or("unread".into(), |b| b.to_string()),
            age
        );
    }
    for issue in &inventory.issues {
        println!("ISSUE\t{issue}");
    }
    let stale = crate::storage::stale_worktrees::stale_worktrees_with(
        root.to_str().unwrap(),
        7,
        &crate::sysload::procscan::working_in,
        false,
        super::current_time(),
    )
    .unwrap();
    for w in &stale.worktrees {
        let target = std::path::Path::new(&w.path).join("src-tauri/target");
        let usage = target.is_dir().then(|| {
            crate::storage::measure_linked_worktree(
                target.to_str().unwrap(),
                "target",
                None,
                std::time::Instant::now() + std::time::Duration::from_secs(20),
            )
        });
        println!(
            "WT\t{}\tprocs={:?}\tclean={:?}\tintegrated={:?}\tidle_days={:?}\tstale={}\ttarget_logical={}\ttarget_private={}\ttarget_truncated={}\t{}",
            w.path,
            w.processes,
            w.clean,
            w.integrated,
            w.idle_days,
            w.stale,
            usage.as_ref().map_or(0, |u| u.bytes),
            usage
                .as_ref()
                .and_then(|u| u.private_bytes)
                .map_or("unread".into(), |b| b.to_string()),
            usage.as_ref().is_some_and(|u| u.truncated),
            w.reasons.join("; ")
        );
    }
}

/// An inspection cut short (here by a cancellation, in the third dry run on
/// 2026-11-14 by the 120-second budget) never showed some scope's candidates,
/// so it is not their preview: a later, calmer run would act on worktrees
/// nobody was shown.
#[cfg(unix)]
#[test]
fn an_inspection_cut_short_does_not_count_as_a_preview() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let service = Cleaner::new(
        crate::engine::git_cli::canonicalize_plain(temp.path())
            .unwrap()
            .join("cut-short-state"),
    );
    let cleaner = Arc::new(service);
    worktree_with_cargo_target(&root);
    let scopes = super::ReclaimScopes {
        prune_worktrees: true,
        ..Default::default()
    };
    let config = scoped(&cleaner, &root, scopes);
    cleaner
        .cancel
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let cut = cleaner.inspect(&config, scopes, true).unwrap();
    assert!(!cut.scopes_complete, "{cut:?}");
    assert_eq!(super::previewed_by(&cut, scopes), None);
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let whole = cleaner.inspect(&config, scopes, true).unwrap();
    assert!(whole.scopes_complete, "{:?}", whole.issues);
    assert_eq!(super::previewed_by(&whole, scopes), Some(scopes));
    assert_eq!(
        super::previewed_by(&whole, super::ReclaimScopes::default()),
        None,
        "nothing enabled is nothing previewed"
    );
}
