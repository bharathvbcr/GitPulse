//! Stress and adversarial coverage for parked-operation detection.
//!
//! The integration suite proves each operation is handled correctly once. This
//! suite attacks the same code with scale, concurrency and malformed state,
//! because detection runs on every repository refresh: a panic, a hang or a
//! wrong answer here is not a cosmetic defect, it is the UI telling a user the
//! repository is clean while it is mid-rebase.

use gitpulse_lib::engine::repo_op::{self, OperationAction, OperationKind};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tempfile::TempDir;

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// For the commands that are supposed to fail because they hit a conflict.
fn try_git(cwd: &Path, args: &[&str]) {
    let _ = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .output()
        .expect("git");
}

fn git_out(cwd: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .expect("git");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    run_git(dir.path(), &["init", "-b", "main"]);
    run_git(dir.path(), &["config", "user.email", "t@example.com"]);
    run_git(dir.path(), &["config", "user.name", "T"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) {
    let dest = dir.join(file);
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(dest, content).unwrap();
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", message]);
}

/// Builds a rebase of `steps` commits whose FIRST replayed commit conflicts,
/// so the sequencer parks with the full plan still recorded.
fn park_long_rebase(dir: &Path, steps: usize) {
    commit(dir, "shared.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    for i in 0..steps {
        // Every step touches the same file so the replay conflicts immediately.
        commit(
            dir,
            "shared.txt",
            &format!("side-{i}\n"),
            &format!("side {i}"),
        );
    }
    run_git(dir, &["checkout", "main"]);
    commit(dir, "shared.txt", "main-diverged\n", "main diverged");
    try_git(dir, &["rebase", "main", "side"]);
}

#[test]
fn a_two_hundred_step_rebase_reports_coherent_progress() {
    let repo = init_repo();
    park_long_rebase(repo.path(), 200);

    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("a 200-step rebase must be detected");
    let current = op.current_step.expect("current step");
    let total = op.total_steps.expect("total steps");
    assert_eq!(total, 200, "every planned step must be counted");
    assert!(
        current >= 1 && current <= total,
        "progress {current}/{total} is incoherent"
    );
    assert!(op.allows(OperationAction::Abort));

    // And it can still be escaped at that scale.
    repo_op::run_action_with(
        &repo.path().to_string_lossy(),
        OperationAction::Abort,
        |_argv| Ok(()),
    )
    .unwrap();
    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
}

/// A merge conflicting in more files than the report will list must carry the
/// true total alongside the capped list. Presenting a capped sample as the
/// whole set is how "3 conflicts left" becomes wrong by three orders of
/// magnitude.
#[test]
fn a_conflict_larger_than_the_listing_cap_reports_the_true_total() {
    const FILES: usize = 1500;
    let repo = init_repo();
    let dir = repo.path();

    for i in 0..FILES {
        fs::write(dir.join(format!("f{i}.txt")), "base\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "base"]);

    run_git(dir, &["checkout", "-b", "side"]);
    for i in 0..FILES {
        fs::write(dir.join(format!("f{i}.txt")), "side\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "side"]);

    run_git(dir, &["checkout", "main"]);
    for i in 0..FILES {
        fs::write(dir.join(format!("f{i}.txt")), "main\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "main"]);

    try_git(dir, &["merge", "--no-edit", "side"]);

    let op = repo_op::detect(dir)
        .unwrap()
        .expect("merge must be detected");
    assert_eq!(op.conflicted_total, FILES, "the true total must be exact");
    assert_eq!(
        op.conflicted_paths.len(),
        1000,
        "the listed sample must stop at the documented cap"
    );
    assert!(
        op.conflicted_paths.len() < op.conflicted_total,
        "a capped sample must be distinguishable from the whole set"
    );
    // Continue stays withheld: the cap must not be mistaken for "resolved".
    assert!(!op.allows(OperationAction::Continue));
}

/// Detection runs on every refresh of every open tab. Hammering it from many
/// threads while the repository is parked must never panic, hang, or produce
/// two different answers for one unchanging state.
#[test]
fn concurrent_detection_is_consistent_under_load() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "f.txt", "side\n", "side");
    run_git(dir, &["checkout", "main"]);
    commit(dir, "f.txt", "main\n", "main");
    try_git(dir, &["merge", "--no-edit", "side"]);

    let expected = repo_op::detect(dir).unwrap();
    assert!(expected.is_some());

    let path = dir.to_path_buf();
    let mismatches = Arc::new(AtomicUsize::new(0));
    let handles: Vec<_> = (0..12)
        .map(|_| {
            let path = path.clone();
            let expected = expected.clone();
            let mismatches = Arc::clone(&mismatches);
            std::thread::spawn(move || {
                for _ in 0..25 {
                    if repo_op::detect(&path).unwrap() != expected {
                        mismatches.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("detection thread must not panic");
    }
    assert_eq!(mismatches.load(Ordering::Relaxed), 0);
    // 300 detections must not have perturbed the repository.
    assert_eq!(repo_op::detect(dir).unwrap(), expected);
}

/// Repeated park/escape cycles must converge every time. A leaked control file
/// would make the app claim an operation is in progress forever, with an abort
/// button that fails.
#[test]
fn repeated_park_and_abort_cycles_always_return_to_idle() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "f.txt", "side\n", "side");
    run_git(dir, &["checkout", "main"]);
    commit(dir, "f.txt", "main\n", "main");
    let settled = git_out(dir, &["rev-parse", "HEAD"]);

    let path = dir.to_string_lossy().into_owned();
    for round in 0..15 {
        // Alternate the operation kind so no single escape path is exercised.
        let argv: &[&str] = match round % 3 {
            0 => &["merge", "--no-edit", "side"],
            1 => &["cherry-pick", "side"],
            _ => &["rebase", "side"],
        };
        try_git(dir, argv);
        let parked = repo_op::detect(dir).unwrap();
        assert!(parked.is_some(), "round {round}: {argv:?} did not park");

        repo_op::run_action_with(&path, OperationAction::Abort, |_argv| Ok(())).unwrap();
        assert_eq!(
            repo_op::detect(dir).unwrap(),
            None,
            "round {round}: abort left the repository parked"
        );
        assert_eq!(
            git_out(dir, &["rev-parse", "HEAD"]),
            settled,
            "round {round}: abort did not restore HEAD"
        );
        assert!(
            git_out(dir, &["status", "--porcelain"]).is_empty(),
            "round {round}: abort left a dirty tree"
        );
    }
}

/// Detection must survive a `.git` whose control files have been corrupted by
/// a crashed tool, a partial sync, or an editor. It may degrade, but it must
/// never panic and must never lose the escape hatch.
#[test]
fn corrupt_control_files_never_panic_and_never_lose_the_escape() {
    let corruptions: &[(&str, &[u8])] = &[
        ("rebase-merge/msgnum", b"\xff\xfe not a number"),
        ("rebase-merge/end", b""),
        ("rebase-merge/head-name", b"\x00\x00\x00"),
        ("rebase-merge/onto", b"not-an-oid-at-all"),
        ("rebase-merge/head-name", b"refs/heads/\n\n\n"),
        ("rebase-merge/onto", b"../../../etc/passwd"),
        ("rebase-merge/msgnum", b"99999999999999999999999999"),
        ("rebase-merge/end", b"-4"),
    ];

    for (relative, bytes) in corruptions {
        let repo = init_repo();
        let dir = repo.path();
        commit(dir, "f.txt", "a\n", "c1");
        commit(dir, "f.txt", "b\n", "c2");
        run_git(dir, &["checkout", "-b", "side", "HEAD~1"]);
        commit(dir, "f.txt", "c\n", "c3");
        try_git(dir, &["rebase", "main"]);

        let target = dir.join(".git").join(relative);
        if !target.exists() {
            // This git version parked on a different backend; nothing to corrupt.
            continue;
        }
        fs::write(&target, bytes).unwrap();

        let detected = repo_op::detect(dir)
            .unwrap_or_else(|e| panic!("{relative} corruption produced a hard error: {e}"));
        let op =
            detected.unwrap_or_else(|| panic!("{relative} corruption hid the operation entirely"));
        assert!(
            op.allows(OperationAction::Abort),
            "{relative}: the escape hatch must survive corruption"
        );
        // Progress may be absent, but it must never be self-contradictory.
        if let (Some(current), Some(total)) = (op.current_step, op.total_steps) {
            assert!(
                current >= 1 && current <= total,
                "{relative}: rendered incoherent progress {current}/{total}"
            );
        }
    }
}

/// Many repositories parked at once, detected concurrently: each answer must
/// belong to its own repository. A shared cache or a common-dir mix-up shows
/// up here as one repo reporting another's operation.
#[test]
fn parallel_repositories_never_report_each_others_operations() {
    let kinds: &[(&str, OperationKind)] = &[
        ("merge", OperationKind::Merge),
        ("cherry-pick", OperationKind::CherryPick),
        ("revert", OperationKind::Revert),
    ];

    let repos: Vec<(TempDir, OperationKind)> = kinds
        .iter()
        .cycle()
        .take(9)
        .map(|(verb, kind)| {
            let repo = init_repo();
            let dir = repo.path();
            // Three commits on main all touching f.txt, plus a diverged side
            // branch. Reverting HEAD~1 then conflicts with HEAD's own change —
            // reverting the tip itself would apply cleanly and never park.
            commit(dir, "f.txt", "v1\n", "c1");
            commit(dir, "f.txt", "v2\n", "c2");
            run_git(dir, &["checkout", "-b", "side", "HEAD~1"]);
            commit(dir, "f.txt", "v3\n", "c3");
            run_git(dir, &["checkout", "main"]);
            commit(dir, "f.txt", "v4\n", "c4");
            match *verb {
                "merge" => try_git(dir, &["merge", "--no-edit", "side"]),
                "cherry-pick" => try_git(dir, &["cherry-pick", "side"]),
                _ => try_git(dir, &["revert", "--no-edit", "HEAD~1"]),
            }
            (repo, *kind)
        })
        .collect();

    let handles: Vec<_> = repos
        .iter()
        .map(|(repo, expected)| {
            let path = repo.path().to_path_buf();
            let expected = *expected;
            std::thread::spawn(move || {
                for _ in 0..15 {
                    let op = repo_op::detect(&path)
                        .unwrap()
                        .expect("each repository is parked");
                    assert_eq!(op.kind, expected, "a repository reported another's state");
                }
            })
        })
        .collect();
    for handle in handles {
        handle.join().expect("no detection thread may panic");
    }
}

/// Paths that break naive parsing must round-trip into the conflicted list.
#[test]
fn adversarial_paths_survive_the_conflicted_listing() {
    let awkward = [
        "plain.txt",
        "with space.txt",
        "with\ttab.txt",
        "ünïcode-ölü.txt",
        "quote'single.txt",
        "dollar$sign.txt",
        "semi;colon.txt",
        "deeply/nested/three/levels/down.txt",
        "trailing.space .txt",
        "-leading-dash.txt",
    ];

    let repo = init_repo();
    let dir = repo.path();
    for name in awkward {
        let dest = dir.join(name);
        fs::create_dir_all(dest.parent().unwrap()).unwrap();
        fs::write(dest, "base\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "base"]);

    run_git(dir, &["checkout", "-b", "side"]);
    for name in awkward {
        fs::write(dir.join(name), "side\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "side"]);

    run_git(dir, &["checkout", "main"]);
    for name in awkward {
        fs::write(dir.join(name), "main\n").unwrap();
    }
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", "main"]);
    try_git(dir, &["merge", "--no-edit", "side"]);

    let op = repo_op::detect(dir)
        .unwrap()
        .expect("merge must be detected");
    assert_eq!(op.conflicted_total, awkward.len());
    for name in awkward {
        assert!(
            op.conflicted_paths.iter().any(|p| p == name),
            "{name:?} was lost or mangled in the conflicted listing; got {:?}",
            op.conflicted_paths
        );
    }
}

/// A bare repository has no worktree and therefore can never be parked. It
/// must answer "idle" rather than erroring the whole status refresh.
#[test]
fn a_bare_repository_reports_idle_rather_than_failing() {
    let dir = TempDir::new().unwrap();
    run_git(dir.path(), &["init", "--bare", "-b", "main"]);
    match repo_op::detect(dir.path()) {
        Ok(None) => {}
        Ok(Some(op)) => panic!("a bare repository cannot be parked, got {:?}", op.kind),
        Err(e) => panic!("a bare repository must not fail detection: {e}"),
    }
}
