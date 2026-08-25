//! Concurrent worktree-tab mutations must serialize on the shared git common
//! dir rather than colliding, and lock/unlock/prune must be live-tested.

use gitpulse_lib::engine::{worktree, GitWriter};
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Barrier};
use std::thread;
use tempfile::TempDir;

fn git_in(dir: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args([
            "-c",
            "user.name=GitPulse",
            "-c",
            "user.email=gitpulse@test.local",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn init_repo() -> TempDir {
    let dir = TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-b", "main"]);
    std::fs::write(dir.path().join("seed.txt"), "seed\n").unwrap();
    git_in(dir.path(), &["add", "."]);
    git_in(dir.path(), &["commit", "-m", "init"]);
    dir
}

#[test]
fn concurrent_mutations_from_linked_worktrees_all_land() {
    let main = init_repo();
    let parent = TempDir::new().unwrap();
    let wt_path = parent.path().join("agent-tab");
    worktree::add_worktree(
        main.path().to_str().unwrap(),
        wt_path.to_str().unwrap(),
        Some("agent/tab"),
        Some("main"),
        false,
    )
    .expect("add worktree");

    let main_path = main.path().to_str().unwrap().to_string();
    let wt_str = wt_path.to_str().unwrap().to_string();
    const PER_SIDE: usize = 6;
    let barrier = Arc::new(Barrier::new(2));
    let main_handle = {
        let path = main_path.clone();
        let barrier = barrier.clone();
        thread::spawn(move || {
            barrier.wait();
            for i in 0..PER_SIDE {
                GitWriter::create_branch(&path, &format!("main-tab-{i}"), None)
                    .unwrap_or_else(|e| panic!("main create_branch {i}: {e}"));
            }
        })
    };
    let wt_handle = {
        let path = wt_str.clone();
        thread::spawn(move || {
            barrier.wait();
            for i in 0..PER_SIDE {
                GitWriter::create_branch(&path, &format!("wt-tab-{i}"), None)
                    .unwrap_or_else(|e| panic!("worktree create_branch {i}: {e}"));
            }
        })
    };
    main_handle.join().expect("main worker");
    wt_handle.join().expect("worktree worker");

    let listed = Command::new("git")
        .args(["branch", "--list"])
        .current_dir(main.path())
        .output()
        .unwrap();
    let stdout = String::from_utf8(listed.stdout).unwrap();
    for i in 0..PER_SIDE {
        assert!(
            stdout.contains(&format!("main-tab-{i}")),
            "missing main-tab-{i} in {stdout}"
        );
        assert!(
            stdout.contains(&format!("wt-tab-{i}")),
            "missing wt-tab-{i} in {stdout}"
        );
    }
}

#[test]
fn lock_unlock_and_prune_from_engine_api() {
    let main = init_repo();
    let parent = TempDir::new().unwrap();
    let wt_path = parent.path().join("to-lock");
    let created = worktree::add_worktree(
        main.path().to_str().unwrap(),
        wt_path.to_str().unwrap(),
        Some("agent/lock"),
        Some("main"),
        false,
    )
    .expect("add");

    worktree::lock_worktree(main.path().to_str().unwrap(), &created, Some("hold")).expect("lock");
    let listed = worktree::list_worktrees(main.path().to_str().unwrap()).expect("list");
    let created_canon = Path::new(&created)
        .canonicalize()
        .unwrap_or_else(|_| Path::new(&created).to_path_buf());
    assert!(
        listed.iter().any(|w| {
            let listed_canon = Path::new(&w.path)
                .canonicalize()
                .unwrap_or_else(|_| Path::new(&w.path).to_path_buf());
            listed_canon == created_canon && w.is_locked
        }),
        "locked worktree missing: created={created}, listed={listed:?}"
    );

    worktree::unlock_worktree(main.path().to_str().unwrap(), &created).expect("unlock");
    std::fs::remove_dir_all(&wt_path).unwrap();
    worktree::prune_worktree(main.path().to_str().unwrap()).expect("prune");
    let listed = worktree::list_worktrees(main.path().to_str().unwrap()).expect("list after prune");
    assert_eq!(listed.len(), 1);
}
