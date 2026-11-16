use super::{
    eligible_candidates, Cleaner, CleanerCandidate, CleanerConfig, CleanerRun, Saved, VERSION,
};
use crate::procguard::LockedSpawn;
use crate::storage::hygiene::discovery;
use std::fs;
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<Cleaner>, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
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
    let walked = crate::engine::process::canonicalize_plain(&repo(&root, "a")).unwrap();
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        evidence: Vec::new(),
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
        evidence: Vec::new(),
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        .status_locked()
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
    assert!(
        run.items
            .iter()
            .any(|item| item.message.contains("active task leases")),
        "{run:?}"
    );
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
        crate::engine::process::canonicalize_plain(temp.path())
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
    let root = crate::engine::process::canonicalize_plain(std::path::Path::new(&root)).unwrap();
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
        ..Default::default()
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
        crate::engine::process::canonicalize_plain(temp.path())
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

/// The worktree veto asks who uses the worktree, not only who stands in it: a
/// build whose working directory is elsewhere and whose arguments name the
/// worktree refuses its removal.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn a_process_naming_a_worktree_refuses_the_worktree_veto() {
    let (_temp, _cleaner, code) = fixture();
    let worktree = repo(&code, "lane");
    let elsewhere = code.join("elsewhere");
    fs::create_dir_all(&elsewhere).unwrap();
    let mut child =
        crate::sysload::procscan::tests::directory_use_child(&worktree, &elsewhere, true);
    let seen = crate::test_support::env::wait_until(std::time::Duration::from_secs(20), || {
        crate::sysload::procscan::processes_using(&worktree).is_ok_and(|pids| !pids.is_empty())
    });
    let verdict = super::quiet_worktree(worktree.to_str().unwrap(), None);
    drop(child.stdin.take());
    let _ = child.kill();
    let _ = child.wait();
    assert!(seen, "the fixture process was never visible to the scan");
    let refused = verdict.expect_err("a process names the worktree");
    assert!(
        refused.message().contains("process(es) use this worktree"),
        "{}",
        refused.message()
    );
}

/// A task store that cannot be read is a lease check that could not run: the
/// refusal is the typed "never judged" kind, not a rule that kept the folder.
#[test]
fn an_unreadable_task_store_is_an_unexamined_lease_check() {
    let (_temp, _cleaner, code) = fixture();
    let worktree = repo(&code, "lane");
    // Ignored, so the worktree reads clean and the lease check is reached.
    fs::write(worktree.join(".git/info/exclude"), ".devcouncil/\n").unwrap();
    fs::create_dir_all(worktree.join(".devcouncil")).unwrap();
    fs::write(
        worktree.join(".devcouncil/state.sqlite"),
        b"this is not a database",
    )
    .unwrap();
    let path = worktree.to_str().unwrap();
    let leases = super::leases_clear(path, None).expect_err("the store cannot be read");
    assert!(super::shed_under_load(&leases), "{}", leases.message());
    assert!(
        leases.message().contains("could not be checked"),
        "{}",
        leases.message()
    );
    let quiet = super::quiet_worktree(path, None).expect_err("the store cannot be read");
    assert!(super::shed_under_load(&quiet), "{}", quiet.message());
}

/// A full or filling disk makes an enabled schedule due, once per gap. Any
/// other reason the host sheds work, a disabled schedule and a run in
/// progress change nothing. It moves the schedule and nothing else: scopes
/// and checks are those the saved policy already names.
#[test]
fn a_disk_cause_brings_the_schedule_forward_once_per_gap() {
    let (_temp, cleaner, root) = fixture();
    let later = super::current_time() + 86_400;
    cleaner
        .update(|saved| {
            saved.config.enabled = true;
            saved.config.roots = vec![root.to_string_lossy().into_owned()];
            saved.config.next_run_at = later;
            Ok(())
        })
        .unwrap();
    let now = super::current_time();
    let next = |c: &Cleaner| c.state().unwrap().config.next_run_at;
    // The argument is `shed_is_disk_only()`: false for any other cause, for a
    // disk shed with something else not clear, and with no governor.
    assert_eq!(cleaner.bring_forward_for_disk(false, now).unwrap(), None);
    assert_eq!(next(&cleaner), later, "not disk-only");
    assert!(
        !crate::sysload::shed_is_disk_only(),
        "this process has no governor, so the scheduler's own call is false"
    );
    let before = cleaner.state().unwrap().config.revision;
    assert_eq!(
        cleaner.bring_forward_for_disk(true, now).unwrap(),
        Some(now)
    );
    assert_eq!(next(&cleaner), now);
    assert_eq!(
        cleaner.state().unwrap().config.revision,
        before,
        "the schedule moving is not an edit of the policy a person's open page is compared against"
    );
    // A save carrying the revision read before the bring-forward still lands.
    let mut open_page = cleaner.state().unwrap().config;
    open_page.next_run_at = later;
    assert!(cleaner.save(open_page).is_ok());
    // Due now, and not asked again within the gap, whatever the cause.
    cleaner
        .update(|saved| {
            saved.config.next_run_at = later;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        cleaner.bring_forward_for_disk(true, now + 60).unwrap(),
        None,
        "bounded: once per gap"
    );
    assert_eq!(
        cleaner
            .bring_forward_for_disk(true, now + super::DISK_FORWARD_MIN_GAP_SECS)
            .unwrap(),
        Some(now + super::DISK_FORWARD_MIN_GAP_SECS)
    );
    // Disabled: nothing moves.
    cleaner
        .update(|saved| {
            saved.config.enabled = false;
            saved.config.next_run_at = later;
            Ok(())
        })
        .unwrap();
    assert_eq!(cleaner.bring_forward_for_disk(true, later).unwrap(), None);
    assert_eq!(next(&cleaner), later);
}

/// Every scope ships off, and a saved policy written before the agent scratch
/// scope existed reads with it off.
#[test]
fn the_agent_scratch_scope_ships_off_and_an_older_policy_reads_it_off() {
    let defaults = super::ReclaimScopes::default();
    assert!(!defaults.agent_scratch_builds);
    assert!(!defaults.any());
    assert!(defaults.is_default());
    let older = r#"{"worktree_caches":true,"per_tree_vetoes":false,"prune_worktrees":false,"worktree_idle_days":7}"#;
    let read: super::ReclaimScopes = serde_json::from_str(older).unwrap();
    assert!(!read.agent_scratch_builds);
    assert_eq!(read.agent_scratch_days, 7);
    // Turning it on is a change a preview must acknowledge before any run
    // uses it.
    let on = super::ReclaimScopes {
        agent_scratch_builds: true,
        ..defaults
    };
    assert!(on.any());
    let now = 1_000_000_000;
    assert!(!on.effective(None, now, now).agent_scratch_builds);
    assert!(!on.effective(Some(defaults), now, now).agent_scratch_builds);
    assert!(on.effective(Some(on), now, now).agent_scratch_builds);
}

/// What an older build (one whose state reader refuses unknown fields) reads.
/// It is the parent's shape: no agent scope, no `previewed_at`, no bring-forward
/// stamp.
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentScopes {
    #[serde(default)]
    worktree_caches: bool,
    #[serde(default)]
    per_tree_vetoes: bool,
    #[serde(default)]
    prune_worktrees: bool,
    #[serde(default)]
    worktree_idle_days: u32,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentConfig {
    version: u32,
    revision: u64,
    roots: Vec<String>,
    exclusions: Vec<String>,
    enabled: bool,
    #[serde(default)]
    run_when_closed: bool,
    interval_hours: u32,
    next_run_at: u64,
    retention_days: u32,
    max_run_bytes: u64,
    max_targets: u32,
    #[serde(default)]
    scopes: Option<ParentScopes>,
    #[serde(default)]
    clone_budget: Option<serde_json::Value>,
}

impl ParentScopes {
    fn read(&self) -> u64 {
        u64::from(self.worktree_caches)
            + u64::from(self.per_tree_vetoes)
            + u64::from(self.prune_worktrees)
            + u64::from(self.worktree_idle_days)
    }
}

impl ParentConfig {
    fn read(&self) -> u64 {
        u64::from(self.version)
            + self.revision
            + self.roots.len() as u64
            + self.exclusions.len() as u64
            + u64::from(self.enabled)
            + u64::from(self.run_when_closed)
            + u64::from(self.interval_hours)
            + self.next_run_at
            + u64::from(self.retention_days)
            + self.max_run_bytes
            + u64::from(self.max_targets)
            + self.scopes.as_ref().map_or(0, ParentScopes::read)
            + u64::from(self.clone_budget.is_some())
    }
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentSaved {
    config: ParentConfig,
    history: Vec<serde_json::Value>,
    cancel_run: Option<String>,
    #[serde(default)]
    background_error: Option<String>,
    #[serde(default)]
    deferral_retries: u32,
    #[serde(default)]
    previewed_scopes: Option<ParentScopes>,
}

impl ParentSaved {
    fn read(&self) -> u64 {
        self.config.read()
            + self.history.len() as u64
            + u64::from(self.cancel_run.is_some())
            + u64::from(self.background_error.is_some())
            + u64::from(self.deferral_retries)
            + self.previewed_scopes.as_ref().map_or(0, ParentScopes::read)
    }
}

/// A state this build writes stays readable by the build before it, until a
/// person turns the new scope on: no agent field, no preview stamp and no
/// bring-forward stamp is written at its default or once it no longer limits
/// anything. Turning the scope on does write them, and the older build then
/// refuses the state, which the docs say.
#[test]
fn state_written_with_the_new_scope_off_is_readable_by_the_parents_reader() {
    let (_temp, cleaner, root) = fixture();
    let read = |cleaner: &Cleaner| -> Result<ParentSaved, String> {
        let bytes = fs::read(cleaner.dir.join("state.json")).map_err(|e| e.to_string())?;
        let parsed: ParentSaved = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        parsed.read();
        Ok(parsed)
    };
    let later = super::current_time() + 86_400;
    cleaner
        .update(|saved| {
            saved.config.enabled = true;
            saved.config.roots = vec![root.to_string_lossy().into_owned()];
            saved.config.next_run_at = later;
            saved.config.scopes.worktree_caches = true;
            saved.previewed_scopes = Some(saved.config.scopes);
            Ok(())
        })
        .unwrap();
    read(&cleaner).expect("old scopes on, new scope off");
    // One bring-forward writes its stamp; it is not written once the gap passed.
    let now = super::current_time();
    cleaner.bring_forward_for_disk(true, now).unwrap();
    assert!(
        read(&cleaner).is_err(),
        "a pending bring-forward is written"
    );
    cleaner
        .bring_forward_for_disk(false, now + super::DISK_FORWARD_MIN_GAP_SECS)
        .unwrap();
    read(&cleaner).expect("the stamp is dropped once its gap has passed");
    // The new scope on writes its fields.
    cleaner
        .update(|saved| {
            saved.config.scopes.agent_scratch_builds = true;
            saved.config.scopes.agent_scratch_days = 14;
            Ok(())
        })
        .unwrap();
    assert!(read(&cleaner).is_err());
}

/// The agent scratch approval is bound to what it was shown, and lapses.
#[test]
fn a_preview_dated_in_the_future_or_never_taken_lapses_like_an_old_one() {
    let scopes = super::ReclaimScopes {
        agent_scratch_builds: true,
        ..super::ReclaimScopes::default()
    };
    let now = 1_800_000_000u64;
    let used = |at: u64| scopes.effective(Some(scopes), at, now).agent_scratch_builds;
    assert!(used(now), "taken now");
    assert!(
        used(now - super::AGENT_PREVIEW_VALID_SECS),
        "exactly 30 days"
    );
    assert!(
        !used(now - super::AGENT_PREVIEW_VALID_SECS - 1),
        "past 30 days"
    );
    assert!(!used(now + 1), "stamped after now: the clock was set back");
    assert!(!used(now + 365 * 86_400), "a year ahead");
    assert!(!used(0), "never previewed by this version");
}

#[test]
fn a_previewed_agent_scope_is_cleared_by_roots_or_exclusions_and_lapses_after_thirty_days() {
    let (_temp, cleaner, root) = fixture();
    // A sibling of the root: a nested one would be folded into it.
    let other = root.parent().unwrap().join("other-root");
    fs::create_dir(&other).unwrap();
    let mut c = config(&cleaner, &root);
    c.scopes.agent_scratch_builds = true;
    let saved = cleaner.save(c).unwrap().config;
    let now = super::current_time();
    let approve = |cleaner: &Cleaner, at: u64| {
        cleaner
            .update(|saved| {
                saved.previewed_scopes = Some(saved.config.scopes);
                saved.previewed_at = at;
                Ok(())
            })
            .unwrap();
    };
    approve(&cleaner, now);
    assert!(cleaner.state().unwrap().scopes_previewed);
    // Changing the roots clears it, then the exclusions, with the scopes alone
    // unchanged.
    let mut c = cleaner.state().unwrap().config;
    c.roots = vec![
        root.to_string_lossy().into_owned(),
        other.to_string_lossy().into_owned(),
    ];
    cleaner.save(c).unwrap();
    assert!(!cleaner.state().unwrap().scopes_previewed, "roots");
    approve(&cleaner, now);
    assert!(cleaner.state().unwrap().scopes_previewed);
    let mut c = cleaner.state().unwrap().config;
    c.exclusions = vec![other.to_string_lossy().into_owned()];
    cleaner.save(c).unwrap();
    assert!(!cleaner.state().unwrap().scopes_previewed, "exclusions");
    // Saving the same roots and exclusions again keeps an approval.
    approve(&cleaner, now);
    let same = cleaner.state().unwrap().config;
    cleaner.save(same).unwrap();
    assert!(cleaner.state().unwrap().scopes_previewed, "unchanged");
    let _ = saved;
    // The lapse is the agent scope's alone, and a lapsed scope is not used.
    let scopes = cleaner.state().unwrap().config.scopes;
    let thirty = 30 * 86_400;
    assert!(
        scopes
            .effective(Some(scopes), now, now + thirty)
            .agent_scratch_builds
    );
    assert!(
        !scopes
            .effective(Some(scopes), now, now + thirty + 1)
            .agent_scratch_builds
    );
    assert!(!scopes.effective(Some(scopes), 0, now).agent_scratch_builds);
    let worktrees = super::ReclaimScopes {
        worktree_caches: true,
        agent_scratch_builds: true,
        ..Default::default()
    };
    let lapsed = worktrees.effective(Some(worktrees), now, now + thirty + 1);
    assert!(lapsed.worktree_caches && !lapsed.agent_scratch_builds);
    approve(&cleaner, now - thirty - 5);
    cleaner
        .update(|saved| {
            saved.config.scopes = worktrees;
            saved.previewed_scopes = Some(worktrees);
            Ok(())
        })
        .unwrap();
    assert!(
        !cleaner.state().unwrap().scopes_previewed,
        "a lapsed approval is not shown as previewed"
    );
}

/// The agent arms of the state veto and of the veto cache key: a session
/// scratchpad has no repository state to read, scratch output in a repository
/// is kept only by an active lease and never by a dirty tree, and the cache
/// key keeps two scratchpads apart while folding a repository's targets.
#[test]
fn the_agent_scopes_have_their_own_state_veto_and_veto_key() {
    let (_temp, cleaner, root) = fixture();
    let dirty = repo(&root, "dirty-repo");
    fs::write(dirty.join("uncommitted.txt"), "work").unwrap();
    let candidate = |scope: &str, repo_path: &Path, path: &str| CleanerCandidate {
        repo_path: repo_path.to_string_lossy().into_owned(),
        path: path.into(),
        provider: String::new(),
        bytes: 1,
        scope: scope.into(),
        private_bytes: None,
        branch: None,
        evidence: vec![],
    };
    let scopes = super::ReclaimScopes::default();
    let session = candidate(
        "agent-session-scratch",
        &root.join("nowhere"),
        "/tmp/claude-1/p/s1/scratchpad/t",
    );
    assert!(cleaner.quiet_tree(&session, scopes).is_ok());
    for scope in ["agent-repo-scratch", "agent-claude-target"] {
        let scratch = candidate(scope, &dirty, &dirty.join("tmp/a/target").to_string_lossy());
        assert!(
            cleaner.quiet_tree(&scratch, scopes).is_ok(),
            "{scope}: a dirty tree elsewhere is no reason to keep ignored scratch output"
        );
    }
    let other = candidate(
        "agent-session-scratch",
        &root,
        "/tmp/claude-1/p/s2/scratchpad/t",
    );
    assert_ne!(
        super::veto_key(&session, scopes),
        super::veto_key(&other, scopes)
    );
    let one = candidate("agent-repo-scratch", &dirty, "/x/a");
    let two = candidate("agent-repo-scratch", &dirty, "/x/b");
    assert_eq!(super::veto_key(&one, scopes), super::veto_key(&two, scopes));
    let claude = candidate("agent-claude-target", &dirty, "/x/c");
    assert_eq!(
        super::veto_key(&claude, scopes),
        super::veto_key(&one, scopes)
    );
}

/// A read-only probe of this machine, never part of a normal run: the real
/// `inspect` with production wiring (real process table, real agent registries,
/// real `/tmp`) over the roots in `GITPULSE_PROBE_ROOTS` (a `:`-separated
/// list), the cleaner's state in a temp directory, and no removal path called.
///
/// What it reads: the process table, the Claude registries under the config
/// directories derived from `GITPULSE_PROBE_HOME` (handed to the detector through
/// `GITPULSE_CLAUDE_CONFIG_DIRS`), the system temp directory and the roots. What
/// it writes: only files in its own temporary directory (the cleaner's state and
/// the test trust store); repository trust is granted in that test store, not the
/// real one. It prints `PROBE` lines (scope completeness, candidates by scope,
/// kept reasons, issues, process counts, and per live session folder whether it
/// came up as a candidate), which is the before/after table a change to the
/// cleaner's rules is judged by; any live session's folder showing as a candidate
/// is a defect.
/// `GITPULSE_PROBE_HOME=$HOME GITPULSE_PROBE_ROOTS=<a>:<b> cargo test --lib -- --ignored --nocapture probe_the_real_machine`.
#[cfg(unix)]
#[test]
#[ignore = "reads the real machine; run by hand"]
fn probe_the_real_machine_with_production_wiring() {
    let roots: Vec<String> = std::env::var("GITPULSE_PROBE_ROOTS")
        .unwrap_or_default()
        .split(':')
        .filter(|r| !r.is_empty())
        .map(str::to_string)
        .collect();
    assert!(!roots.is_empty(), "set GITPULSE_PROBE_ROOTS");
    // The test build runs under a hermetic HOME, so the Claude config
    // directories production would resolve with the real one are resolved here
    // from `GITPULSE_PROBE_HOME` and handed to the detector. Reads only.
    let real_home = std::env::var("GITPULSE_PROBE_HOME").expect("set GITPULSE_PROBE_HOME");
    let dirs = crate::workbench::conversation::config_dirs_in(
        None,
        None,
        None,
        Some(Path::new(&real_home)),
    );
    eprintln!("PROBE config dirs: {:?} error={:?}", dirs.dirs, dirs.error);
    std::env::set_var(
        "GITPULSE_CLAUDE_CONFIG_DIRS",
        std::env::join_paths(&dirs.dirs).unwrap(),
    );
    let temp = tempfile::tempdir().unwrap();
    let state_dir = crate::engine::process::canonicalize_plain(temp.path())
        .unwrap()
        .join("state");
    let cleaner = Cleaner::new(state_dir);
    let mut config = cleaner.state().unwrap().config;
    config.roots = roots;
    config.scopes.agent_scratch_builds = true;
    // The test build keeps its own repository-trust store, so none of the
    // owner's repositories is trusted in it. Trusting what discovery finds
    // here changes only that test store, never the real one, and stands for
    // the owner having opened and trusted these repositories in the app.
    let probe_roots = discovery::roots(&config.roots, false).unwrap();
    let grant = |dir: &Path| {
        let plain = dir.to_string_lossy().into_owned();
        if let Ok(preview) = crate::repository_trust::inspect(&plain) {
            let _ = crate::repository_trust::grant(&plain, &preview.identity, false);
        }
    };
    for root in &probe_roots {
        grant(root);
    }
    for _ in 0..3 {
        let found = discovery::discover(&probe_roots, &[], &AtomicBool::new(false));
        for repo in &found.repos {
            grant(repo);
        }
    }
    let started = std::time::Instant::now();
    let inventory = cleaner.inspect(&config, config.scopes, true).unwrap();
    let elapsed = started.elapsed();
    eprintln!(
        "PROBE elapsed={elapsed:?} scopes_complete={} partial={} repositories={} candidates={} kept={} shed_repos={}",
        inventory.scopes_complete,
        inventory.partial,
        inventory.repositories,
        inventory.candidates.len(),
        inventory.kept.len(),
        inventory.shed_repos
    );
    let mut by_scope = std::collections::BTreeMap::<String, usize>::new();
    for c in &inventory.candidates {
        *by_scope.entry(c.scope.clone()).or_default() += 1;
    }
    eprintln!("PROBE candidates by scope: {by_scope:?}");
    for c in inventory
        .candidates
        .iter()
        .filter(|c| c.scope.starts_with("agent-"))
    {
        eprintln!("PROBE CANDIDATE {} {} {}", c.scope, c.bytes, c.path);
    }
    let mut reasons = std::collections::BTreeMap::<String, usize>::new();
    for k in &inventory.kept {
        let head: String = k.reason.chars().take(60).collect();
        *reasons.entry(format!("{} | {head}", k.scope)).or_default() += 1;
    }
    for (reason, n) in &reasons {
        eprintln!("PROBE KEPT x{n}: {reason}");
    }
    for issue in inventory.issues.iter().take(40) {
        let head: String = issue.chars().take(220).collect();
        eprintln!("PROBE ISSUE {head}");
    }
    let table = crate::sysload::procscan::read_use_table().unwrap();
    let mine: Vec<_> = table.iter().filter(|p| p.mine).collect();
    eprintln!(
        "PROBE processes total={} mine={} mine_cwd_unreadable={} mine_argv_unreadable={} mine_env_unreadable={} mine_env_empty={}",
        table.len(),
        mine.len(),
        mine.iter().filter(|p| p.cwd.is_none()).count(),
        mine.iter().filter(|p| p.argv.is_none()).count(),
        mine.iter().filter(|p| p.env.is_none()).count(),
        mine.iter()
            .filter(|p| p.env.as_ref().is_some_and(Vec::is_empty))
            .count()
    );
    let live = super::super::agent::LiveSessions::observe();
    eprintln!(
        "PROBE live claude ids={} unidentified={} incomplete={:?}",
        live.ids.len(),
        live.unidentified,
        live.incomplete
    );
    // What the unidentified sessions are, by shape only: executable, flag
    // names and the names (never values) of session-related variables.
    for kind in crate::insights::live_sessions::observe()
        .iter()
        .filter(|k| k.kind == "claude")
    {
        eprintln!(
            "PROBE claude detector ok={} live={} unverified={}",
            kind.ok,
            kind.live.len(),
            kind.unverified.len()
        );
        for session in kind.live.iter().filter(|s| s.session_id.is_none()) {
            let row = table
                .iter()
                .find(|p| i64::from(p.pid) == i64::from(session.pid));
            let flags: Vec<String> = row
                .and_then(|p| p.argv.as_ref())
                .map(|argv| {
                    argv.iter()
                        .filter(|a| a.starts_with('-'))
                        .map(|a| a.split('=').next().unwrap_or("").to_string())
                        .collect()
                })
                .unwrap_or_default();
            let names: Vec<String> = row
                .and_then(|p| p.env.as_ref())
                .map(|env| {
                    env.iter()
                        .filter_map(|e| e.split_once('=').map(|(n, _)| n.to_string()))
                        .filter(|n| {
                            let upper = n.to_uppercase();
                            upper.contains("SESSION") || upper.starts_with("CLAUDE")
                        })
                        .collect()
                })
                .unwrap_or_default();
            eprintln!(
                "PROBE UNIDENTIFIED pid={} entrypoint={:?} status={:?} tree={:?} exe={:?} flags={flags:?} env_names={names:?}",
                session.pid,
                session.entrypoint,
                session.status,
                session.tree,
                row.and_then(|p| p.exe.clone())
            );
        }
    }
    // Every live session's folder must read as in use, never as a candidate.
    let base = Path::new("/private/tmp/claude-501");
    let mut live_as_candidate = 0;
    if let Ok(projects) = fs::read_dir(base) {
        for project in projects.flatten() {
            for id in live.ids.iter() {
                let folder = project.path().join(id);
                if !folder.is_dir() {
                    continue;
                }
                let as_candidate = inventory
                    .candidates
                    .iter()
                    .any(|c| Path::new(&c.path).starts_with(&folder));
                let kept: Vec<&str> = inventory
                    .kept
                    .iter()
                    .filter(|k| Path::new(&k.path).starts_with(&folder))
                    .map(|k| k.reason.as_str())
                    .collect();
                eprintln!(
                    "PROBE LIVE {} candidate={as_candidate} kept={:?}",
                    folder.display(),
                    kept.iter()
                        .map(|r| r.chars().take(70).collect::<String>())
                        .collect::<Vec<_>>()
                );
                if as_candidate {
                    live_as_candidate += 1;
                }
            }
        }
    }
    eprintln!("PROBE live sessions whose folder is a candidate: {live_as_candidate}");
}

/// A Cargo target as Cargo writes one, every entry last modified 30 days ago.
fn old_cargo_target(dir: &std::path::Path) {
    fs::create_dir_all(dir.join("debug")).unwrap();
    fs::write(
        dir.join("CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();
    fs::write(dir.join(".rustc_info.json"), "{}").unwrap();
    fs::write(dir.join("debug/.cargo-lock"), "").unwrap();
    fs::write(dir.join("debug/app.o"), vec![b'x'; 4096]).unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
    for path in [
        dir.join("debug/app.o"),
        dir.join("debug/.cargo-lock"),
        dir.join("CACHEDIR.TAG"),
        dir.join(".rustc_info.json"),
        dir.join("debug"),
        dir.to_path_buf(),
    ] {
        fs::File::open(path)
            .unwrap()
            .set_times(fs::FileTimes::new().set_modified(when))
            .unwrap();
    }
}

/// Preview, then run, through `inventory` and `perform_with_gate` with the
/// injected gate: scope 1 (a finished session's scratchpad under a stand-in
/// temp root), scope 2 (a linked worktree's `.gitpulse/scratch`, which
/// discovery never reaches) and scope 3 (`.claude` targets in the checkout and
/// in the worktree). Only the targets go; the scratchpad, `.claude/skills` and
/// every other file stay. A lapsed approval skips the scope and says so.
#[cfg(unix)]
#[test]
fn agent_scratch_builds_are_previewed_then_removed_end_to_end() {
    use crate::sysload::procscan::DirUse;
    let _serial = crate::storage::hygiene::tests::serial();
    let (temp, _old, root) = fixture();
    let temp_root = crate::engine::process::canonicalize_plain(temp.path())
        .unwrap()
        .join("faketmp");
    fs::create_dir_all(&temp_root).unwrap();
    let mut service = Cleaner::new(
        crate::engine::process::canonicalize_plain(temp.path())
            .unwrap()
            .join("agent-state"),
    );
    service.activity_override = Some(|_| Ok(()));
    service.agent_machine = Some(super::AgentMachine {
        live: super::super::agent::LiveSessions::default,
        use_of: |_| Ok(DirUse::default()),
        temp_root: temp_root.clone(),
    });
    let cleaner = Arc::new(service);
    let (repo, wt) = worktree_with_cargo_target(&root);
    fs::write(repo.join(".git/info/exclude"), ".gitpulse/\ntmp/\n").unwrap();
    let scratch_in_worktree = wt.join(".gitpulse/scratch/target-a");
    old_cargo_target(&scratch_in_worktree);
    fs::write(wt.join(".gitpulse/scratch/notes.txt"), "keep").unwrap();
    let claude_main = repo.join(".claude/x-target");
    old_cargo_target(&claude_main);
    fs::create_dir_all(repo.join(".claude/skills")).unwrap();
    fs::write(repo.join(".claude/skills/a.md"), "skill").unwrap();
    let claude_wt = wt.join(".claude/y-target");
    old_cargo_target(&claude_wt);
    let uid = super::super::agent::current_uid();
    let pad = temp_root.join(format!(
        "claude-{uid}/proj/session-0001-aaaaaaaa/scratchpad"
    ));
    let session_target = pad.join("target-s");
    old_cargo_target(&session_target);
    fs::write(pad.join("notes.txt"), "keep").unwrap();
    quiet_session_folder(&pad);

    let mut c = config(&cleaner, &root);
    c.scopes.agent_scratch_builds = true;
    cleaner.save(c).unwrap();
    let preview = cleaner.inventory().unwrap();
    let scopes: std::collections::BTreeSet<&str> = preview
        .candidates
        .iter()
        .map(|c| c.scope.as_str())
        .collect();
    for scope in [
        "agent-session-scratch",
        "agent-repo-scratch",
        "agent-claude-target",
    ] {
        assert!(scopes.contains(scope), "{scope}: {preview:?}");
    }
    let offered: Vec<&str> = preview.candidates.iter().map(|c| c.path.as_str()).collect();
    for expected in [
        &scratch_in_worktree,
        &claude_main,
        &claude_wt,
        &session_target,
    ] {
        assert!(
            offered.iter().any(|p| Path::new(p) == expected.as_path()),
            "{} not offered: {offered:?} / kept {:?} / issues {:?}",
            expected.display(),
            preview.kept,
            preview.issues
        );
    }
    assert!(
        cleaner.state().unwrap().scopes_previewed,
        "{:?}",
        preview.issues
    );

    // A lapsed approval: nothing is removed and the run says why.
    let day = 86_400;
    cleaner
        .update(|saved| {
            saved.previewed_at = super::current_time() - 31 * day;
            Ok(())
        })
        .unwrap();
    let saved = cleaner.state().unwrap().config;
    cleaner
        .cancel
        .store(false, std::sync::atomic::Ordering::SeqCst);
    let mut lapsed = journaled_run(&cleaner, "lapsed-run");
    {
        let _run_lock = cleaner.lock("run.lock").unwrap();
        // With nothing eligible and part of the repository unread the run may
        // end in the ordinary refusal; nothing is removed either way.
        let _ = cleaner.perform_with_gate(&saved, &mut lapsed, |_, _, _| {
            panic!("a lapsed scope offers nothing to gate")
        });
    }
    assert!(
        lapsed.issues.iter().any(|i| i.contains("30 days")),
        "{:?}",
        lapsed.issues
    );
    assert!(session_target.is_dir() && claude_main.is_dir());

    // A fresh preview approves it again, and the run removes exactly them.
    cleaner.inventory().unwrap();
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "agent-run");
    for gone in [
        &scratch_in_worktree,
        &claude_main,
        &claude_wt,
        &session_target,
    ] {
        assert!(!gone.exists(), "{} survived: {run:?}", gone.display());
    }
    assert!(
        pad.join("notes.txt").is_file(),
        "the scratchpad's files stay"
    );
    assert!(wt.join(".gitpulse/scratch/notes.txt").is_file());
    assert!(repo.join(".claude/skills/a.md").is_file());
    assert!(wt.join("Cargo.toml").is_file());
}

/// Makes a session folder look like a finished one's: its own time, its
/// scratchpad's and what is directly in the scratchpad 30 days old (a build
/// folder's contents are aged by the fixture that made it).
fn quiet_session_folder(scratchpad: &std::path::Path) {
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(30 * 86_400);
    let set = |p: &std::path::Path| {
        if let Ok(file) = fs::File::open(p) {
            let _ = file.set_times(fs::FileTimes::new().set_modified(when));
        }
    };
    for entry in fs::read_dir(scratchpad).into_iter().flatten().flatten() {
        set(&entry.path());
    }
    set(scratchpad);
    if let Some(session) = scratchpad.parent() {
        set(session);
    }
}

/// A cleaner whose home, Claude configuration directories, process table and
/// `/tmp` are the test's, with one removable scratch build in a worktree.
fn long_lived_fixture() -> (
    tempfile::TempDir,
    Arc<Cleaner>,
    std::path::PathBuf,
    std::path::PathBuf,
    std::path::PathBuf,
) {
    use crate::sysload::procscan::DirUse;
    let (temp, _old, root) = fixture();
    let base = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
    let home = base.join("home");
    let temp_root = base.join("faketmp");
    fs::create_dir_all(&temp_root).unwrap();
    fs::create_dir_all(home.join(".cache/huggingface/hub")).unwrap();
    fs::write(
        home.join(".cache/huggingface/hub/model.bin"),
        vec![b'm'; 4096],
    )
    .unwrap();
    let mut service = Cleaner::new(base.join("ll-state"));
    service.activity_override = Some(|_| Ok(()));
    service.agent_machine = Some(super::AgentMachine {
        live: super::super::agent::LiveSessions::default,
        use_of: |_| Ok(DirUse::default()),
        temp_root,
    });
    service.long_lived_machine = Some((home.clone(), vec![home.join(".claude")]));
    let cleaner = Arc::new(service);
    let (repo, wt) = worktree_with_cargo_target(&root);
    fs::write(repo.join(".git/info/exclude"), ".gitpulse/\ntmp/\n").unwrap();
    let scratch = wt.join(".gitpulse/scratch/target-a");
    old_cargo_target(&scratch);
    let mut c = config(&cleaner, &root);
    c.scopes.agent_scratch_builds = true;
    cleaner.save(c).unwrap();
    (temp, cleaner, root, repo, scratch)
}

fn candidate_paths(inventory: &super::CleanerInventory) -> Vec<String> {
    let mut paths: Vec<String> = inventory
        .candidates
        .iter()
        .map(|c| c.path.clone())
        .collect();
    paths.sort();
    paths
}

/// Whatever the long-lived list names, including a directory inside a reclaim
/// root and the very folder a scope offers, it is only reported: the candidates
/// are the ones the scopes alone would offer, a reported directory that is not a
/// candidate survives a run, and nothing the run records names it.
#[test]
fn long_lived_data_is_reported_and_never_a_candidate_or_removed() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, _root, repo, scratch) = long_lived_fixture();
    let baseline = cleaner.inventory().unwrap();
    assert!(!baseline.candidates.is_empty(), "{baseline:?}");
    assert!(
        baseline
            .long_lived
            .iter()
            .any(|r| r.label == "Hugging Face cache" && r.bytes == Some(4096)),
        "{:?}",
        baseline.long_lived
    );
    let keep = repo.join("data-store");
    fs::create_dir_all(&keep).unwrap();
    fs::write(keep.join("precious.db"), "do not delete").unwrap();
    let mut c = cleaner.state().unwrap().config;
    c.long_lived_paths = vec![
        keep.to_string_lossy().into_owned(),
        scratch.to_string_lossy().into_owned(),
    ];
    cleaner.save(c).unwrap();
    let preview = cleaner.inventory().unwrap();
    assert_eq!(
        candidate_paths(&preview),
        candidate_paths(&baseline),
        "the list neither adds nor removes a candidate"
    );
    let keep_text = keep.to_string_lossy().into_owned();
    assert!(
        !preview
            .candidates
            .iter()
            .any(|c| c.path == keep_text || Path::new(&c.path).starts_with(&keep)),
        "{preview:?}"
    );
    assert!(preview
        .long_lived
        .iter()
        .any(|r| r.path == keep_text && r.bytes == Some(13)));
    let saved = cleaner.state().unwrap().config;
    let run = run_now(&cleaner, &saved, "long-lived-run");
    assert!(
        !scratch.exists(),
        "the scope's own candidate is still removed: {run:?}"
    );
    assert_eq!(
        fs::read_to_string(keep.join("precious.db")).unwrap(),
        "do not delete"
    );
    assert!(
        !format!("{run:?}").contains("data-store"),
        "a reported path is not an item of the run: {run:?}"
    );
}

/// A walk that could not read part of a path is a floor in the report, and it
/// leaves the approval of the scope exactly as the same preview without it.
#[cfg(unix)]
#[test]
fn a_cut_short_long_lived_walk_is_a_floor_and_leaves_the_approval_alone() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, _root, repo, _scratch) = long_lived_fixture();
    let without = cleaner.inventory().unwrap();
    assert!(cleaner.state().unwrap().scopes_previewed);
    let data = repo.join("big-data");
    fs::create_dir_all(data.join("sealed")).unwrap();
    fs::write(data.join("a.bin"), vec![b'a'; 100]).unwrap();
    fs::write(data.join("sealed/b.bin"), vec![b'b'; 900]).unwrap();
    let mut c = cleaner.state().unwrap().config;
    c.long_lived_paths = vec![data.to_string_lossy().into_owned()];
    cleaner.save(c).unwrap();
    let _sealed = deny(&data.join("sealed"));
    let preview = cleaner.inventory().unwrap();
    let row = preview
        .long_lived
        .iter()
        .find(|r| r.path == data.to_string_lossy())
        .unwrap();
    assert!(row.at_least && row.bytes == Some(100), "{row:?}");
    assert!(
        cleaner.state().unwrap().scopes_previewed,
        "the approval stands"
    );
    assert_eq!(preview.partial, without.partial);
    assert_eq!(preview.scopes_complete, without.scopes_complete);
    assert_eq!(candidate_paths(&preview), candidate_paths(&without));
}

/// The list is validated where it is saved, with the reason.
#[test]
fn the_long_lived_list_is_validated_when_saved() {
    let (_temp, cleaner, root) = fixture();
    let home = root.parent().unwrap().join("home");
    fs::create_dir_all(&home).unwrap();
    let mut service = Cleaner::new(root.parent().unwrap().join("v-state"));
    service.long_lived_machine = Some((home.clone(), vec![]));
    let _ = cleaner;
    let save = |paths: Vec<String>| {
        let mut c = config(&service, &root);
        c.long_lived_paths = paths;
        service.save(c)
    };
    let ok = home.join("models");
    fs::create_dir_all(&ok).unwrap();
    assert!(save(vec![ok.to_string_lossy().into_owned()]).is_ok());
    for (path, why) in [
        ("relative", "absolute"),
        ("/", "filesystem root"),
        (home.to_str().unwrap(), "home directory"),
        (root.to_str().unwrap(), "contains the repository root"),
        ("/definitely/not/here", "cannot be read"),
    ] {
        let error = save(vec![path.to_string()]).expect_err(path);
        assert!(error.message().contains(why), "{path}: {}", error.message());
    }
    let too_many: Vec<String> = (0..=super::super::long_lived::MAX_USER_PATHS)
        .map(|n| format!("/x/{n}"))
        .collect();
    assert!(save(too_many).is_err());
}

/// A state with no long-lived paths is readable by the build before the field
/// existed; adding one writes a field that build refuses, like a scope.
#[test]
fn an_empty_long_lived_list_keeps_the_state_readable_by_the_older_build() {
    let (_temp, cleaner, root) = fixture();
    let read = |cleaner: &Cleaner| -> Result<ParentSaved, String> {
        let bytes = fs::read(cleaner.dir.join("state.json")).map_err(|e| e.to_string())?;
        serde_json::from_slice(&bytes).map_err(|e| e.to_string())
    };
    let data = root.join("data");
    fs::create_dir_all(&data).unwrap();
    let mut c = config(&cleaner, &root);
    cleaner.save(c.clone()).unwrap();
    read(&cleaner).expect("an empty list writes no field");
    c = cleaner.state().unwrap().config;
    c.long_lived_paths = vec![data.to_string_lossy().into_owned()];
    cleaner.save(c.clone()).unwrap();
    assert!(
        read(&cleaner).is_err(),
        "a listed path writes a field the older build refuses"
    );
    c = cleaner.state().unwrap().config;
    c.long_lived_paths.clear();
    cleaner.save(c).unwrap();
    read(&cleaner).expect("emptied again, readable again");
}

static TOUCH_AT_CALL: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static USE_CALLS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static UNREADABLE_FROM_CALL: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// The process table as these end-to-end tests drive it: unreadable on demand,
/// and, at the numbered call, a build touching the session's `.rustc_info.json`
/// (a file the removal unlinks last), which is how a removal is made to stop
/// part way. Statics because the machine seam is a plain function.
fn scripted_use(dir: &Path) -> Result<crate::sysload::procscan::DirUse, String> {
    use std::sync::atomic::Ordering;
    let call = USE_CALLS.fetch_add(1, Ordering::SeqCst) + 1;
    let from = UNREADABLE_FROM_CALL.load(Ordering::SeqCst);
    if from > 0 && call >= from {
        return Err("the process table could not be read".into());
    }
    if call == TOUCH_AT_CALL.load(Ordering::SeqCst) {
        let _ = fs::write(
            dir.join("scratchpad/target-a/.rustc_info.json"),
            "{\"touched\":true}",
        );
    }
    Ok(crate::sysload::procscan::DirUse::default())
}

/// Two finished sessions, each with a 4,141-byte build folder, a cleaner with
/// the agent scope on and previewed, and the scripted process table.
fn two_session_cleaner(cap: u64) -> (tempfile::TempDir, Arc<Cleaner>, Vec<std::path::PathBuf>) {
    use std::sync::atomic::Ordering;
    TOUCH_AT_CALL.store(0, Ordering::SeqCst);
    USE_CALLS.store(0, Ordering::SeqCst);
    UNREADABLE_FROM_CALL.store(0, Ordering::SeqCst);
    let (temp, _old, root) = fixture();
    let base = crate::engine::process::canonicalize_plain(temp.path()).unwrap();
    let temp_root = base.join("faketmp");
    let uid = super::super::agent::current_uid();
    let mut targets = Vec::new();
    for id in ["session-0001-aaaaaaaa", "session-0002-bbbbbbbb"] {
        let pad = temp_root.join(format!("claude-{uid}/proj/{id}/scratchpad"));
        let target = pad.join("target-a");
        old_cargo_target(&target);
        quiet_session_folder(&pad);
        targets.push(target);
    }
    let mut service = Cleaner::new(base.join("two-state"));
    service.activity_override = Some(|_| Ok(()));
    service.agent_machine = Some(super::AgentMachine {
        live: super::super::agent::LiveSessions::default,
        use_of: scripted_use,
        temp_root,
    });
    service.long_lived_machine = Some((base.join("home"), vec![]));
    let cleaner = Arc::new(service);
    let mut c = config(&cleaner, &root);
    c.scopes.agent_scratch_builds = true;
    c.max_run_bytes = cap;
    cleaner.save(c).unwrap();
    let preview = cleaner.inventory().unwrap();
    assert_eq!(
        preview
            .candidates
            .iter()
            .filter(|c| c.scope == "agent-session-scratch")
            .count(),
        2,
        "{preview:?}"
    );
    (temp, cleaner, targets)
}

/// A removal that stops at 90% leaves its bytes gone, and they count: the run
/// charges them, records the row as partly removed (not skipped), and the next
/// candidate meets what is left of the cap. Charging only completed removals let
/// a second 40 GB candidate in under a 50 GB cap.
#[test]
fn a_partly_removed_candidate_is_charged_and_the_next_one_meets_the_cap() {
    use std::sync::atomic::Ordering;
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, targets) = two_session_cleaner(6_000);
    let saved = cleaner.state().unwrap().config;
    USE_CALLS.store(0, Ordering::SeqCst);
    // Calls 1 and 2 are the run's own inspection; 3 is the first delete-time
    // re-judgement and 4 the re-check right before its first unlink.
    TOUCH_AT_CALL.store(4, Ordering::SeqCst);
    let run = run_now(&cleaner, &saved, "partial-run");
    let session_rows: Vec<_> = run
        .items
        .iter()
        .filter(|i| i.path.contains("target-a"))
        .collect();
    let partial: Vec<_> = session_rows
        .iter()
        .filter(|i| i.status == "partially_removed")
        .collect();
    assert_eq!(partial.len(), 1, "{:?}", run.items);
    assert!(partial[0].bytes_before >= 4096, "{:?}", partial[0]);
    // The size after is unknown, not zero: the row must not read as emptied.
    assert_eq!(partial[0].bytes_after, None, "{:?}", partial[0]);
    assert!(partial[0]
        .message
        .contains("count against the run's byte limit"));
    let refused: Vec<_> = session_rows
        .iter()
        .filter(|i| i.status == "skipped")
        .collect();
    assert_eq!(refused.len(), 1, "{:?}", run.items);
    assert!(
        refused[0]
            .message
            .contains("exceeds the remaining byte budget"),
        "{:?}",
        refused[0]
    );
    let intact = targets
        .iter()
        .filter(|t| t.join("debug/app.o").exists())
        .count();
    assert_eq!(intact, 1, "the second candidate was never touched");
    assert_eq!(run.status, "completed_with_skips");
}

/// A check that could not run at delete time is an unexamined row, counted, and
/// nothing is removed: the process table unreadable after a good preview.
#[test]
fn a_check_that_could_not_run_at_delete_time_is_an_unexamined_row() {
    use std::sync::atomic::Ordering;
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, targets) = two_session_cleaner(10 * 1024 * 1024 * 1024);
    let saved = cleaner.state().unwrap().config;
    // The run's own inspection reads the table twice (one candidate each); the
    // third reading is the first delete-time re-judgement, and every later one
    // fails.
    USE_CALLS.store(0, Ordering::SeqCst);
    UNREADABLE_FROM_CALL.store(3, Ordering::SeqCst);
    let run = run_now(&cleaner, &saved, "unexamined-run");
    UNREADABLE_FROM_CALL.store(0, Ordering::SeqCst);
    let rows: Vec<_> = run
        .items
        .iter()
        .filter(|i| i.path.contains("target-a"))
        .collect();
    assert_eq!(rows.len(), 2, "{:?}", run.items);
    for row in rows {
        assert_eq!(row.status, "unexamined", "{row:?}");
        assert!(row.message.contains("Could not examine"), "{row:?}");
    }
    assert!(run.unexamined >= 2, "{run:?}");
    assert!(
        targets.iter().all(|t| t.join("debug/app.o").exists()),
        "nothing was removed"
    );
}

/// A due scheduled run waits while the host sheds background work for a reason
/// other than the disk, runs when the disk alone is the reason, and runs when
/// nothing is shed. With no governor in the process (the headless run) it never
/// waits. Through the real accessors, with a governor that has history.
#[test]
fn a_due_scheduled_run_waits_for_every_pressure_but_the_disk() {
    use crate::sysload::{inject_governor_with_history, GovernorInput, Level, Reading, Readings};
    let history =
        |input: GovernorInput| inject_governor_with_history(std::iter::repeat_n(input, 10));
    let low_disk = || GovernorInput {
        readings: Readings {
            free_disk: Reading::Value(8 << 30),
            ..Readings::calm()
        },
        ..GovernorInput::test_pressure(Level::Normal)
    };
    assert!(
        !super::scheduled_run_waits(),
        "no governor in this thread: it runs"
    );
    {
        let _memory = history(GovernorInput::test_pressure(Level::Critical));
        assert!(crate::sysload::shed().background, "the fixture sheds");
        assert!(super::scheduled_run_waits(), "memory Severe: it waits");
    }
    {
        let _disk = history(low_disk());
        assert!(crate::sysload::shed().background);
        assert!(!super::scheduled_run_waits(), "the disk alone: it runs");
    }
    {
        let mut both = low_disk();
        both.level = Some(Level::Critical);
        let _both = history(both);
        assert!(super::scheduled_run_waits(), "disk and memory: it waits");
    }
    {
        let _clear = history(GovernorInput::test_pressure(Level::Normal));
        assert!(!super::scheduled_run_waits(), "nothing shed: it runs");
    }
}

/// Only a preview that measures reports; a run, which does not, carries none.
#[test]
fn a_run_inspection_does_not_build_the_long_lived_list() {
    let _serial = crate::storage::hygiene::tests::serial();
    let (_temp, cleaner, _root, _repo, _scratch) = long_lived_fixture();
    let config = cleaner.state().unwrap().config;
    let run_view = cleaner.inspect(&config, config.scopes, false).unwrap();
    assert!(run_view.long_lived.is_empty(), "{:?}", run_view.long_lived);
    assert!(!cleaner.inventory().unwrap().long_lived.is_empty());
}
