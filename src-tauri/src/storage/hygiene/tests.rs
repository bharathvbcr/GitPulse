use super::{
    cancel, execute_with_activity, invocation, local_checks, managed_cache_path, plans,
    prepare_with_activity, providers, tree, HygienePlan, EXECUTION, MAX_PLANS, TTL,
};
use std::fs::{self, File, FileTimes};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub(super) static TEST_LOCK: Mutex<()> = Mutex::new(());

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let repo = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
    crate::engine::git_cli::git_global(&["init", "-q", repo.to_str().unwrap()]).unwrap();
    fs::write(
        repo.join(".gitignore"),
        "target/\ntarget-audit/\n__pycache__/\n",
    )
    .unwrap();
    fs::write(
        repo.join("Cargo.toml"),
        "[package]\nname='fixture'\nversion='1.2.3'\nedition='2021'\n",
    )
    .unwrap();
    fs::create_dir_all(repo.join("target/debug")).unwrap();
    fs::write(
        repo.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55\n",
    )
    .unwrap();
    fs::write(repo.join("target/.rustc_info.json"), "{}").unwrap();
    fs::write(repo.join("target/debug/output"), "generated").unwrap();
    age(&repo.join("target"));
    (temp, repo)
}

fn age(path: &Path) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            age(&entry.unwrap().path());
        }
    }
    let old = SystemTime::now() - Duration::from_secs(100 * 86400);
    let times = FileTimes::new().set_modified(old);
    #[cfg(windows)]
    if path.is_dir() {
        // File::open on a directory is ERROR_INVALID_FUNCTION (os error 1).
        // FILE_FLAG_BACKUP_SEMANTICS is required to set a directory mtime.
        use std::os::windows::fs::OpenOptionsExt;
        File::options()
            .write(true)
            .custom_flags(0x0200_0000)
            .open(path)
            .unwrap()
            .set_times(times)
            .unwrap();
        return;
    }
    File::open(path).unwrap().set_times(times).unwrap();
}

fn preview(repo: &Path) -> HygienePlan {
    prepare_with_activity(repo.to_str().unwrap(), "local:target", 30, |_| Ok(())).unwrap()
}

#[test]
#[cfg(unix)]
fn plan_executes_exact_reviewed_target_once_and_keeps_source() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    let view = preview(&repo);
    let (policy, outcome) = execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |argv, path| {
            assert!(argv.is_none());
            assert_eq!(path, repo.join("target").to_str().unwrap());
            Ok("actual gate verdict")
        },
        |_| Ok(()),
    )
    .unwrap();
    assert_eq!(policy, "actual gate verdict");
    assert!(outcome.success, "{}", outcome.message);
    assert_eq!(outcome.bytes_after, Some(0));
    assert!(!repo.join("target").exists());
    assert!(repo.join("Cargo.toml").exists());
    assert!(repo.join(".git").exists());
    assert!(
        execute_with_activity(repo.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(())).is_err()
    );
}

#[test]
fn stale_expired_cancelled_and_foreign_plans_never_mutate() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    let (_other, other) = fixture();
    let view = preview(&repo);
    assert!(
        execute_with_activity(other.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(()))
            .unwrap_err()
            .contains("another")
    );
    fs::write(repo.join("target/debug/output"), "changed").unwrap();
    assert!(
        execute_with_activity(repo.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(()))
            .unwrap_err()
            .contains("changed")
    );
    age(&repo.join("target"));
    let view = preview(&repo);
    plans()
        .lock()
        .unwrap()
        .pending
        .get_mut(&view.id)
        .unwrap()
        .created = Instant::now() - TTL;
    assert!(
        execute_with_activity(repo.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(()))
            .unwrap_err()
            .contains("expired")
    );
    let view = preview(&repo);
    cancel(repo.to_str().unwrap(), &view.id).unwrap();
    assert!(
        execute_with_activity(repo.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(())).is_err()
    );
    assert_eq!(
        fs::read_to_string(repo.join("target/debug/output")).unwrap(),
        "changed"
    );
}

#[test]
fn gate_denial_busy_activity_and_post_gate_writes_fail_closed() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    assert!(
        prepare_with_activity(repo.to_str().unwrap(), "local:target", 30, |_| Err(
            "busy".into()
        ))
        .is_err()
    );
    let view = preview(&repo);
    assert_eq!(
        execute_with_activity::<()>(
            repo.to_str().unwrap(),
            &view.id,
            |_, _| Err("policy denied".into()),
            |_| Ok(())
        )
        .unwrap_err(),
        "policy denied"
    );
    assert!(repo.join("target/debug/output").exists());
    let view = preview(&repo);
    assert!(execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |_, _| {
            fs::write(repo.join("target/debug/new"), "new work").unwrap();
            Ok(())
        },
        |_| Ok(())
    )
    .unwrap_err()
    .contains("changed"));
    assert!(repo.join("target/debug/new").exists());
}

#[test]
fn cancellation_and_concurrent_execution_are_enforced_during_policy_review() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    let view = preview(&repo);
    let guard = EXECUTION.lock().unwrap();
    assert!(
        execute_with_activity(repo.to_str().unwrap(), &view.id, |_, _| Ok(()), |_| Ok(()))
            .unwrap_err()
            .contains("Another hygiene operation")
    );
    drop(guard);
    let error = execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |_, _| {
            cancel(repo.to_str().unwrap(), &view.id)?;
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("Cancelled"), "{error}");
    assert!(repo.join("target/debug/output").exists());
    assert!(!plans().lock().unwrap().active.contains_key(&view.id));
}

#[test]
fn plan_budget_is_bounded_and_cancellation_releases_capacity() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    let views: Vec<_> = (0..MAX_PLANS).map(|_| preview(&repo)).collect();
    assert!(
        prepare_with_activity(repo.to_str().unwrap(), "local:target", 30, |_| Ok(()))
            .unwrap_err()
            .contains("Too many")
    );
    for view in views {
        cancel(repo.to_str().unwrap(), &view.id).unwrap();
    }
    let view = preview(&repo);
    cancel(repo.to_str().unwrap(), &view.id).unwrap();
}

#[test]
fn index_and_ignore_changes_during_policy_review_preserve_output() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    let view = preview(&repo);
    let error = execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |_, _| {
            crate::engine::git_cli::git_text(&repo, &["add", "-f", "target/debug/output"])?;
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("Tracked"), "{error}");
    crate::engine::git_cli::git_text(&repo, &["rm", "--cached", "target/debug/output"]).unwrap();
    let view = preview(&repo);
    let error = execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |_, _| {
            fs::write(repo.join(".gitignore"), "").unwrap();
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap_err();
    assert!(error.contains("ignore"), "{error}");
    assert!(repo.join("target/debug/output").exists());
}

#[test]
fn persistent_state_nested_in_generated_output_is_preserved() {
    let (_temp, repo) = fixture();
    for name in [".claude", "node_modules", "logs", ".terraform", ".venv"] {
        let path = repo.join("target/debug").join(name);
        fs::create_dir(&path).unwrap();
        fs::write(path.join("state"), "keep").unwrap();
        assert!(
            tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).is_err(),
            "{name}"
        );
        fs::remove_dir_all(path).unwrap();
    }
}

#[test]
fn ecosystem_adapters_require_their_producer_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path();
    for (name, manifest, label) in [
        (".next", "package.json", "JavaScript / TypeScript"),
        (".nuxt", "package.json", "JavaScript / TypeScript"),
        (".output", "package.json", "JavaScript / TypeScript"),
        (".svelte-kit", "package.json", "JavaScript / TypeScript"),
        (".parcel-cache", "package.json", "JavaScript / TypeScript"),
        (".turbo", "package.json", "JavaScript / TypeScript"),
        (".vite", "package.json", "JavaScript / TypeScript"),
        ("build", "build.gradle.kts", "JVM / Gradle"),
        ("target", "pom.xml", "JVM / Maven"),
        ("obj", "App.csproj", ".NET intermediates"),
    ] {
        assert!(providers::local_provider(repo, name).is_err(), "{name}");
        fs::write(repo.join(manifest), "producer").unwrap();
        assert_eq!(providers::local_provider(repo, name).unwrap(), label);
        fs::remove_file(repo.join(manifest)).unwrap();
    }
    for name in [".pytest_cache", ".mypy_cache", ".ruff_cache"] {
        fs::create_dir(repo.join(name)).unwrap();
        assert!(providers::local_provider(repo, name).is_err());
        fs::write(
            repo.join(name).join("CACHEDIR.TAG"),
            "Signature: 8a477f597d28d172789f06886806bc55",
        )
        .unwrap();
        assert_eq!(
            providers::local_provider(repo, name).unwrap(),
            "Python tooling"
        );
    }
    fs::create_dir(repo.join(".gocache")).unwrap();
    assert!(providers::local_provider(repo, ".gocache").is_err());
    fs::write(
        repo.join(".gocache/README"),
        "This directory holds cached build artifacts from the Go build system.\n",
    )
    .unwrap();
    assert_eq!(
        providers::local_provider(repo, ".gocache").unwrap(),
        "Go build cache"
    );
    fs::create_dir(repo.join("cmake-build-debug")).unwrap();
    fs::write(repo.join("CMakeLists.txt"), "producer").unwrap();
    assert!(providers::local_provider(repo, "cmake-build-debug").is_err());
    fs::write(repo.join("cmake-build-debug/CMakeCache.txt"), "generated").unwrap();
    assert_eq!(
        providers::local_provider(repo, "cmake-build-debug").unwrap(),
        "C / C++ / CMake"
    );
    assert!(providers::local_provider(repo, "dist").is_err());
}

#[test]
fn retention_paths_git_index_and_marker_checks_are_independent() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    for path in [
        "",
        ".",
        "..",
        "../target",
        "/tmp",
        "target/../../",
        "target\n",
        "target\\outside",
    ] {
        assert!(tree::relative_path(path).is_err(), "{path}");
    }
    for days in [0, 3651, u32::MAX] {
        assert!(
            prepare_with_activity(repo.to_str().unwrap(), "local:target", days, |_| Ok(()))
                .is_err()
        );
    }
    fs::write(repo.join("target/debug/output"), "new").unwrap();
    assert!(
        prepare_with_activity(repo.to_str().unwrap(), "local:target", 30, |_| Ok(()))
            .unwrap_err()
            .contains("modified")
    );
    age(&repo.join("target"));
    crate::engine::git_cli::git_text(&repo, &["add", "-f", "target/debug/output"]).unwrap();
    assert!(local_checks(&repo, "target")
        .unwrap_err()
        .contains("Tracked"));
    crate::engine::git_cli::git_text(&repo, &["rm", "--cached", "target/debug/output"]).unwrap();
    fs::write(repo.join(".gitignore"), "").unwrap();
    assert!(local_checks(&repo, "target").is_err());
    fs::remove_file(repo.join("target/CACHEDIR.TAG")).unwrap();
    assert!(providers::local_provider(&repo, "target").is_err());
}

#[test]
fn nested_repos_secrets_models_and_non_bytecode_are_preserved() {
    let (_temp, repo) = fixture();
    for name in [".env", "weights.gguf", "private.key", "pyvenv.cfg"] {
        let path = repo.join("target/debug").join(name);
        fs::write(&path, "preserved").unwrap();
        assert!(tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).is_err());
        fs::remove_file(path).unwrap();
    }
    fs::create_dir_all(repo.join("target/dependency/.git")).unwrap();
    assert!(tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).is_err());
    fs::create_dir_all(repo.join("__pycache__")).unwrap();
    fs::write(repo.join("__pycache__/source.py"), "source").unwrap();
    assert!(tree::snapshot(&repo.join("__pycache__"), true, &AtomicBool::new(false)).is_err());
    for path in [
        ".venv",
        ".claude",
        ".gradle",
        "node_modules",
        "nested/.gopath",
        "logs",
    ] {
        assert!(providers::protected_artifact(path));
    }
}

#[cfg(unix)]
#[test]
fn symlink_escape_replacement_and_new_files_survive_removal() {
    use std::os::unix::fs::symlink;
    let (_temp, repo) = fixture();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("precious"), "keep").unwrap();
    symlink(outside.path(), repo.join("target/debug/escape")).unwrap();
    assert!(tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).is_err());
    fs::remove_file(repo.join("target/debug/escape")).unwrap();
    let snapshot = tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).unwrap();
    fs::rename(repo.join("target/debug"), repo.join("saved-debug")).unwrap();
    symlink(outside.path(), repo.join("target/debug")).unwrap();
    assert!(
        tree::remove_reviewed(&repo.join("target"), &snapshot, &AtomicBool::new(false)).is_err()
    );
    assert_eq!(
        fs::read_to_string(outside.path().join("precious")).unwrap(),
        "keep"
    );
    fs::remove_file(repo.join("target/debug")).unwrap();
    fs::rename(repo.join("saved-debug"), repo.join("target/debug")).unwrap();
    let snapshot = tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).unwrap();
    fs::write(repo.join("target/unreviewed"), "keep").unwrap();
    assert!(
        tree::remove_reviewed(&repo.join("target"), &snapshot, &AtomicBool::new(false)).is_err()
    );
    assert!(repo.join("target/unreviewed").exists());
}

#[test]
fn snapshot_budgets_cancellation_and_age_are_honest() {
    let (_temp, repo) = fixture();
    assert!(tree::snapshot(&repo.join("target"), true, &AtomicBool::new(true)).is_err());
    let snapshot = tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).unwrap();
    assert!(tree::require_age(&snapshot, 30, UNIX_EPOCH).is_err());
    assert!(
        tree::remove_reviewed(&repo.join("target"), &snapshot, &AtomicBool::new(true)).is_err()
    );
    let mut path = repo.join("target");
    for _ in 0..50 {
        path.push("nested");
    }
    fs::create_dir_all(path).unwrap();
    assert!(
        tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false))
            .unwrap_err()
            .contains("budget")
    );
}

#[test]
fn shared_cache_commands_pin_scope_and_preserve_downloads() {
    let home = Path::new("/Users/example");
    let path = home.join("Library/Caches/go-build");
    let go = invocation("go", &path, home).unwrap();
    assert_eq!(go.argv, ["go", "clean", "-cache"]);
    assert!(go
        .environment
        .contains(&("GOCACHE".into(), path.to_string_lossy().into_owned())));
    assert_eq!(
        invocation("npm", &path, home).unwrap().argv,
        ["npm", "--cache", path.to_str().unwrap(), "cache", "verify"]
    );
    assert_eq!(
        invocation("uv", &path, home).unwrap().argv.last().unwrap(),
        "prune"
    );
    assert_eq!(
        invocation("pnpm", &path, home)
            .unwrap()
            .argv
            .last()
            .unwrap(),
        "prune"
    );
    for id in ["cargo", "go-modules", "unknown"] {
        assert!(invocation(id, &path, home).is_err());
    }
    let temp = tempfile::tempdir().unwrap();
    let home = crate::engine::git_cli::canonicalize_plain(temp.path()).unwrap();
    assert!(managed_cache_path(&home, &home).is_err());
    fs::create_dir_all(home.join("Library/Caches/go-build")).unwrap();
    assert!(managed_cache_path(&home.join("Library/Caches/go-build"), &home).is_ok());
    fs::create_dir_all(home.join("Library/Caches/.git")).unwrap();
    assert!(managed_cache_path(&home.join("Library/Caches/go-build"), &home).is_err());
}

/// Exercises the real owning tool against a disposable cache, without
/// discovering or touching the user's shared cache. Kept opt-in because Go
/// is not a build dependency of GitPulse or every CI host.
#[test]
#[cfg(unix)]
#[ignore = "requires installed Go; run explicitly to verify native cache maintenance"]
fn installed_go_cleans_only_the_reviewed_temporary_cache() {
    let _serial = TEST_LOCK.lock().unwrap();
    let (_temp, repo) = fixture();
    fs::write(repo.join(".gitignore"), ".gocache/\n").unwrap();
    fs::write(repo.join("main.go"), "package main\nfunc main() {}\n").unwrap();
    let cache = repo.join(".gocache");
    let env = [
        ("GOCACHE", cache.to_str().unwrap()),
        ("GOTOOLCHAIN", "local"),
        ("GOWORK", "off"),
        ("GO111MODULE", "off"),
        ("GOPROXY", "off"),
        ("GOSUMDB", "off"),
        ("CGO_ENABLED", "0"),
    ];
    let build = crate::engine::git_cli::capture_command(
        "go",
        &["build", "-o", "fixture-app", "main.go"],
        Some(&repo),
        Duration::from_secs(60),
        &env,
    )
    .expect("installed Go executes");
    assert!(
        build.success,
        "Go fixture build failed: {}",
        build.stderr_text()
    );
    age(&cache);
    let view =
        prepare_with_activity(repo.to_str().unwrap(), "local:.gocache", 30, |_| Ok(())).unwrap();
    assert!(view.bytes > 0);
    let (_, result) = execute_with_activity(
        repo.to_str().unwrap(),
        &view.id,
        |argv, path| {
            assert_eq!(argv.unwrap(), ["go", "clean", "-cache"]);
            assert_eq!(Path::new(path), cache);
            Ok(())
        },
        |_| Ok(()),
    )
    .unwrap();
    assert!(result.success, "{}", result.message);
    assert!(result.bytes_after.unwrap() < result.bytes_before);
    assert!(repo.join("main.go").is_file());
    assert!(repo.join("fixture-app").is_file());
}

#[test]
fn differently_cased_credentials_and_known_model_formats_are_preserved() {
    let (_temp, repo) = fixture();
    for name in [
        ".ENV.production",
        "CREDENTIALS.JSON",
        "weights.GGUF",
        "weights.pt",
        "model.onnx",
        "dataset.parquet",
    ] {
        let path = repo.join("target/debug").join(name);
        fs::write(&path, "preserve").unwrap();
        assert!(
            tree::snapshot(&repo.join("target"), true, &AtomicBool::new(false)).is_err(),
            "{name}"
        );
        fs::remove_file(path).unwrap();
    }
}
