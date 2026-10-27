//! Adversarial probes of the commit-graph write (b17/9cfa4874), each a
//! regression test of the break it found.

use super::*;
use crate::ipc_error::CommandError;
use crate::procguard::LockedSpawn;
use crate::storage::commit_graph::CommitGraphState;
use crate::test_support::{git_in, git_repo};
use std::process::Command;

fn text(path: &Path) -> String {
    path.to_str().unwrap().to_string()
}

fn click(repo: &str, confirmed: &[String]) -> Result<CommitGraphWriteOutcome, CommandError> {
    crate::engine::git_cli::mark_user_action(repo);
    GitWriter::commit_graph_write(repo, confirmed)
}

fn large_history(count: usize) -> (tempfile::TempDir, PathBuf) {
    let dir = git_repo();
    let mut stream = String::new();
    for index in 0..count {
        let body = format!("{index}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@example.test> {} +0000\ndata 3\nc{}\n",
            1_700_000_000 + index,
            index % 10
        ));
        stream.push_str(&format!(
            "M 644 inline d{}/f{}.txt\ndata {}\n{body}\n",
            index % 7,
            index % 50,
            body.len()
        ));
    }
    git_with_stdin(dir.path(), &["fast-import", "--quiet"], stream.as_bytes())
        .expect("fast-import builds the history");
    git_in(dir.path(), &["reset", "-q", "--hard", "main"]);
    let path = dir.path().canonicalize().unwrap();
    (dir, path)
}

fn wait_for_flight(common: &Path) {
    let deadline = std::time::Instant::now()
        + crate::test_support::coverage_relaxed(std::time::Duration::from_secs(20));
    while !commit_graph_cancels().lock().unwrap().contains_key(common) {
        assert!(
            std::time::Instant::now() < deadline,
            "the write never started"
        );
        std::thread::sleep(std::time::Duration::from_micros(200));
    }
}

/// Another git (gc, `fetch` with `fetch.writeCommitGraph`, a second GitPulse)
/// takes `commit-graph.lock` after our snapshot of "locks that were there
/// before" and before our git takes it. Our git then fails on the lock, and the
/// sweep removes every lock "absent before": the foreign one.
#[test]
fn adv_a_foreign_lock_taken_after_the_snapshot_is_not_swept() {
    let (_dir, repo) = large_history(30_000);
    let argv = GitWriter::commit_graph_write_argv();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    let lock = repo.join(".git/objects/info/commit-graph.lock");
    let running = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    wait_for_flight(&common);
    // Git computes the commit list and Bloom filters before it takes the lock,
    // so this lands first; if it did not, the fixture is wrong, not the lane.
    std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&lock)
        .expect("setup: our git took the lock before the foreign one");
    let result = running.join().unwrap();
    assert!(
        result.is_err(),
        "our git cannot have written past a held lock: {result:?}"
    );
    assert!(
        lock.exists(),
        "a lock another git process created after the snapshot was removed"
    );
    let message = result.unwrap_err();
    assert!(
        message.contains("A lock file remains"),
        "{message}",
        message = message.message()
    );
    assert!(
        message.contains(&lock.display().to_string()),
        "{message}",
        message = message.message()
    );
    assert!(
        message.contains("GitPulse never removes"),
        "{message}",
        message = message.message()
    );
}

/// A partial clone: `--reachable --changed-paths` reads trees, and without
/// `GIT_NO_LAZY_FETCH` git fetches the missing ones from the promisor remote.
/// That is network traffic the preview never named.
#[test]
fn adv_a_write_in_a_partial_clone_fetches_nothing() {
    let source = git_repo();
    for index in 0..4 {
        std::fs::create_dir_all(source.path().join(format!("d{index}"))).unwrap();
        std::fs::write(
            source.path().join(format!("d{index}/f")),
            format!("{index}\n"),
        )
        .unwrap();
        git_in(source.path(), &["add", "."]);
        git_in(source.path(), &["commit", "-q", "-m", &format!("c{index}")]);
    }
    git_in(source.path(), &["config", "uploadpack.allowFilter", "true"]);
    git_in(
        source.path(),
        &["config", "uploadpack.allowAnySHA1InWant", "true"],
    );
    let holder = tempfile::TempDir::new().unwrap();
    let clone = holder.path().join("clone");
    let status = Command::new("git")
        .args(["clone", "-q", "--filter=tree:0", "--no-checkout"])
        .arg(format!("file://{}", source.path().display()))
        .arg(&clone)
        .status_locked()
        .expect("clone");
    assert!(status.success());
    crate::test_support::trust_repo(&clone);
    let clone = clone.canonicalize().unwrap();
    let in_pack = |repo: &Path| -> String {
        let out = Command::new("git")
            .args(["count-objects", "-v"])
            .current_dir(repo)
            .output_locked()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|line| line.starts_with("in-pack") || line.starts_with("packs"))
            .collect::<Vec<_>>()
            .join(",")
    };
    let before = in_pack(&clone);
    // Whatever the lane decides (refuse the plan, or let git fail), the
    // remote must not be contacted.
    if let Ok(plan) = GitWriter::commit_graph_write_plan(&text(&clone)) {
        let _ = click(&text(&clone), &plan.argv);
    }
    assert_eq!(
        in_pack(&clone),
        before,
        "the write fetched objects from the promisor remote"
    );
}

/// A cancel that lands after git has already published the graph and exited
/// must not be reported as "Stopped. Nothing was written."
#[test]
fn adv_a_cancel_after_publish_is_not_reported_as_nothing_written() {
    let argv = GitWriter::commit_graph_write_argv();
    for round in 0..40 {
        let (_dir, repo) = large_history(2_000);
        let graph = repo.join(".git/objects/info/commit-graph");
        let running = {
            let (repo, argv) = (text(&repo), argv.clone());
            std::thread::spawn(move || click(&repo, &argv))
        };
        while !graph.exists() {
            if running.is_finished() {
                break;
            }
            std::hint::spin_loop();
        }
        let _ = GitWriter::commit_graph_write_cancel(&text(&repo));
        let outcome = running.join().unwrap().expect("outcome");
        assert!(
            !(outcome.cancelled && graph.exists()),
            "round {round}: reported cancelled / nothing written, yet the graph is on disk"
        );
    }
}

/// A linked worktree and the main checkout share one common dir, so they must
/// share one write; the same repository through a symlinked path must too.
#[cfg(unix)]
#[test]
fn adv_a_linked_worktree_and_a_symlinked_path_join_one_write() {
    let (_dir, repo) = large_history(30_000);
    let holder = tempfile::TempDir::new().unwrap();
    let linked = holder.path().join("linked");
    git_in(&repo, &["worktree", "add", "-q", linked.to_str().unwrap()]);
    crate::test_support::trust_repo(&linked);
    let alias = holder.path().join("alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
    let argv = GitWriter::commit_graph_write_argv();
    let common = crate::repository_trust::git_directories(&repo).unwrap().1;
    let leader = {
        let (repo, argv) = (text(&repo), argv.clone());
        std::thread::spawn(move || click(&repo, &argv))
    };
    wait_for_flight(&common);
    let via_linked = {
        let (path, argv) = (text(&linked.canonicalize().unwrap()), argv.clone());
        std::thread::spawn(move || click(&path, &argv))
    };
    let via_alias = {
        let (path, argv) = (text(&alias), argv.clone());
        std::thread::spawn(move || click(&path, &argv))
    };
    let leader = leader.join().unwrap().expect("leader");
    let via_linked = via_linked.join().unwrap().expect("linked");
    let via_alias = via_alias.join().unwrap().expect("alias");
    assert!(!leader.joined);
    assert!(via_linked.joined, "the linked worktree ran its own git");
    assert!(via_alias.joined, "the symlinked path ran its own git");
    assert_eq!(leader.report.state, CommitGraphState::Present);
}

/// Config posture that held: no write offered where git ignores or refuses the
/// graph, no config written where it is offered, and sha256 round-trips.
#[test]
fn adv_config_posture_holds() {
    let repo_with = |setup: &dyn Fn(&Path)| {
        let dir = git_repo();
        for index in 0..3 {
            std::fs::write(dir.path().join("f"), format!("{index}\n")).unwrap();
            git_in(dir.path(), &["add", "f"]);
            git_in(dir.path(), &["commit", "-q", "-m", &format!("c{index}")]);
        }
        setup(dir.path());
        let path = dir.path().canonicalize().unwrap();
        (dir, path)
    };
    let (_a, off) = repo_with(&|p| git_in(p, &["config", "core.commitGraph", "false"]));
    let refused =
        GitWriter::commit_graph_write_plan(&text(&off)).expect_err("core.commitGraph=false");
    assert!(
        refused.contains("Unknown"),
        "{refused}",
        refused = refused.message()
    );
    let (_b, shallow) = repo_with(&|p| std::fs::write(p.join(".git/shallow"), "").unwrap());
    assert!(GitWriter::commit_graph_write_plan(&text(&shallow)).is_err());
    let (_c, fetchy) = repo_with(&|p| git_in(p, &["config", "fetch.writeCommitGraph", "true"]));
    let before = std::fs::read(fetchy.join(".git/config")).unwrap();
    let plan = GitWriter::commit_graph_write_plan(&text(&fetchy)).expect("offered");
    click(&text(&fetchy), &plan.argv).expect("write");
    assert_eq!(std::fs::read(fetchy.join(".git/config")).unwrap(), before);

    let dir = tempfile::TempDir::new().unwrap();
    git_in(
        dir.path(),
        &["init", "-q", "--object-format=sha256", "-b", "main"],
    );
    crate::test_support::trust_repo(dir.path());
    std::fs::write(dir.path().join("f"), "1\n").unwrap();
    git_in(dir.path(), &["add", "f"]);
    git_in(dir.path(), &["commit", "-q", "-m", "c"]);
    let sha = dir.path().canonicalize().unwrap();
    let plan = GitWriter::commit_graph_write_plan(&text(&sha)).expect("sha256 offered");
    let outcome = click(&text(&sha), &plan.argv).expect("sha256 write");
    assert_eq!(
        outcome.report.state,
        CommitGraphState::Present,
        "{:?}",
        outcome.report
    );
}
