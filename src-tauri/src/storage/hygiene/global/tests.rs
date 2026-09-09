use super::{Cleaner, CleanerConfig, CleanerRun, Saved, VERSION};
use crate::storage::hygiene::discovery;
use std::fs;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

fn fixture() -> (tempfile::TempDir, Arc<Cleaner>, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
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
    repo
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
        assert!(state.history.iter().all(|r| r.status == "completed"));
    }
    assert_eq!(cleaner.state().unwrap().history.len(), 20);
}

#[cfg(unix)]
#[test]
fn global_cleanup_uses_real_snapshots_and_removal_with_exact_limits() {
    // Only OS process activity is injected: the test itself runs under Cargo.
    // Discovery, Git status/index, journal and removal are real. The policy
    // service is injected at the same gate called by production.
    let _serial = crate::storage::hygiene::tests::TEST_LOCK.lock().unwrap();
    let (temp, _old, root) = fixture();
    let mut service = Cleaner::new(temp.path().canonicalize().unwrap().join("execution-state"));
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
    let _serial = crate::storage::hygiene::tests::TEST_LOCK.lock().unwrap();
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
    use std::os::unix::fs::PermissionsExt;
    let (_temp, cleaner, root) = fixture();
    let r = repo(&root, "repo");
    let hidden = r.join(".hidden");
    fs::create_dir(&hidden).unwrap();
    fs::write(hidden.join("source"), "preserve").unwrap();
    cleaner.save(config(&cleaner, &root)).unwrap();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o0)).unwrap();
    let report = cleaner.inventory();
    fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        report.unwrap().partial,
        "an inaccessible storage subtree is not a complete scan"
    );
}
