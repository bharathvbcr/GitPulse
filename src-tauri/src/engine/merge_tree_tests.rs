//! Data-safety tests for lane integration in the object database: the target
//! ref moves only when every checkout that has it can follow, and a checkout
//! never ends up with HEAD, index and files disagreeing.

use crate::engine::git_cli::git_text;
use crate::engine::merge_tree::HeldReason;
use crate::engine::worktree::merge_and_teardown_requiring;
use crate::engine::worktree::{
    add_worktree_extended, merge_and_teardown_worktree, MergeTeardownError, MergeTeardownResult,
};
use crate::ipc_error::CommandError;
use std::path::{Path, PathBuf};

fn git(cwd: &Path, args: &[&str]) -> String {
    git_text(cwd, args)
        .unwrap_or_else(|e| panic!("git {args:?}: {}", e.message()))
        .trim()
        .to_string()
}

fn allow_all(_: &[&str]) -> Result<(), CommandError> {
    Ok(())
}

fn init_repo(dir: &Path) {
    crate::test_support::git_in(dir, &["init", "-q", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@gitpulse.local"]);
    git(dir, &["config", "user.name", "GitPulse Tester"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    git(dir, &["add", "README.md"]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    crate::test_support::trust_repo(dir);
}

/// A repository on `main` and a lane `feature-x` with one commit that edits
/// README.md (so merging it moves the checkout's files).
fn lane_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let lane = tempfile::tempdir().unwrap();
    add_worktree_extended(
        dir.path().to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("feature-x"),
        None,
        false,
        false,
        false,
    )
    .unwrap();
    crate::test_support::trust_repo(lane.path());
    std::fs::write(lane.path().join("README.md"), "hello\nlane line\n").unwrap();
    git(lane.path(), &["commit", "-q", "-am", "lane edits README"]);
    (dir, lane)
}

fn merge(
    repo: &Path,
    lane: &Path,
    target: &str,
    squash: bool,
    gate: &mut dyn FnMut(&[&str]) -> Result<(), CommandError>,
) -> Result<MergeTeardownResult, MergeTeardownError> {
    merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some(target),
        squash,
        None,
        gate,
    )
}

fn staged(repo: &Path) -> String {
    git(repo, &["diff", "--cached", "--name-status"])
}

fn symbolic_head(repo: &Path) -> String {
    git(repo, &["symbolic-ref", "-q", "HEAD"])
}

fn branch_exists(repo: &Path, name: &str) -> bool {
    git_text(
        repo,
        &["rev-parse", "--verify", &format!("refs/heads/{name}")],
    )
    .is_ok()
}

/// A tracked file whose bytes are unchanged but whose mtime moved (an editor
/// save, a formatter). GitPulse's status runs without optional locks, so it
/// calls the checkout clean without refreshing the index, and `read-tree -m -u`
/// would then refuse "not uptodate" after the ref had already moved. The
/// index is refreshed first, so the integration lands and the checkout follows.
#[test]
fn a_stat_dirty_but_clean_checkout_follows_the_ref() {
    for touch_at in ["merge-tree", "update-ref"] {
        let (dir, lane) = lane_fixture();
        let repo = dir.path();
        let result = merge(repo, lane.path(), "main", false, &mut |argv: &[&str]| {
            if argv.first() == Some(&touch_at) {
                let file = std::fs::File::options()
                    .write(true)
                    .open(repo.join("README.md"))
                    .unwrap();
                file.set_modified(std::time::SystemTime::now() + std::time::Duration::from_secs(5))
                    .unwrap();
            }
            Ok(())
        })
        .unwrap_or_else(|e| panic!("touch at {touch_at}: {e}"));
        assert!(result.checkout_advanced, "touch at {touch_at}: {result:?}");
        assert_eq!(symbolic_head(repo), "refs/heads/main");
        assert_eq!(staged(repo), "", "touch at {touch_at}");
        assert_eq!(git(repo, &["status", "--porcelain"]), "");
        assert_eq!(
            std::fs::read_to_string(repo.join("README.md")).unwrap(),
            "hello\nlane line\n"
        );
    }
}

/// `read-tree` fails after the compare-and-swap (here `index.lock` appears
/// between the cleanliness check and the sync). The ref goes back, so HEAD, the
/// index and the files still agree, and the lane is kept.
#[test]
fn a_checkout_that_cannot_follow_puts_the_ref_back() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    let before = git(repo, &["rev-parse", "refs/heads/main"]);
    let lock = repo.join(".git/index.lock");
    let refused = merge(repo, lane.path(), "main", false, &mut |argv: &[&str]| {
        if argv.first() == Some(&"read-tree") {
            std::fs::write(&lock, "").unwrap();
        }
        Ok(())
    })
    .unwrap_err();
    std::fs::remove_file(&lock).ok();
    let message = refused.message().to_string();
    assert!(message.contains("nothing was merged"), "{message}");
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert_eq!(symbolic_head(repo), "refs/heads/main");
    assert_eq!(staged(repo), "");
    assert_eq!(git(repo, &["status", "--porcelain"]), "");
    assert!(lane.path().exists() && branch_exists(repo, "feature-x"));
}

/// A checkout whose `index.lock` is already held is refused before the ref
/// moves, naming the checkout.
#[test]
fn a_held_index_lock_refuses_before_anything_moves() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    let before = git(repo, &["rev-parse", "refs/heads/main"]);
    std::fs::write(repo.join(".git/index.lock"), "").unwrap();
    let refused = merge(repo, lane.path(), "main", false, &mut allow_all).unwrap_err();
    std::fs::remove_file(repo.join(".git/index.lock")).ok();
    let MergeTeardownError::Held { reason, path, .. } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(reason, HeldReason::CheckoutLocked);
    assert!(path.is_some());
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(lane.path().exists() && branch_exists(repo, "feature-x"));
}

/// An overlapping dirty main checkout is not detached and not moved: the
/// integration is refused with a typed reason and nothing changes, so a live
/// session in it keeps its branch and its uncommitted edit.
#[test]
fn a_dirty_main_checkout_refuses_and_stays_attached() {
    for squash in [false, true] {
        let (dir, lane) = lane_fixture();
        let repo = dir.path();
        std::fs::write(repo.join("README.md"), "in progress\n").unwrap();
        let before = git(repo, &["rev-parse", "refs/heads/main"]);
        let porcelain = git(repo, &["status", "--porcelain"]);
        let refused = merge(repo, lane.path(), "main", squash, &mut allow_all).unwrap_err();
        let MergeTeardownError::Held {
            reason,
            path,
            message,
        } = refused
        else {
            panic!("squash={squash}: {refused:?}");
        };
        assert_eq!(reason, HeldReason::CheckoutDirty, "{message}");
        assert!(path.is_some(), "{message}");
        assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
        assert_eq!(symbolic_head(repo), "refs/heads/main", "squash={squash}");
        assert_eq!(git(repo, &["status", "--porcelain"]), porcelain);
        assert!(lane.path().exists() && branch_exists(repo, "feature-x"));
    }
}

/// The target is checked out in a sibling worktree, not the main checkout.
/// `git branch -f` refuses that; `update-ref` does not, so the engine looks.
fn sibling_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    PathBuf,
    tempfile::TempDir,
    PathBuf,
) {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let sibling_dir = tempfile::tempdir().unwrap();
    let sibling = sibling_dir.path().join("integ");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "integ",
            sibling.to_str().unwrap(),
        ],
    );
    crate::test_support::trust_repo(&sibling);
    let lane_dir = tempfile::tempdir().unwrap();
    let lane = lane_dir.path().join("lane");
    git(
        dir.path(),
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "feature-y",
            lane.to_str().unwrap(),
            "integ",
        ],
    );
    crate::test_support::trust_repo(&lane);
    std::fs::write(lane.join("y.txt"), "y\n").unwrap();
    git(&lane, &["add", "y.txt"]);
    git(&lane, &["commit", "-q", "-m", "y"]);
    (dir, sibling_dir, sibling, lane_dir, lane)
}

#[test]
fn a_clean_sibling_checkout_of_the_target_follows_the_ref() {
    let (dir, _sibling_dir, sibling, _lane_dir, lane) = sibling_fixture();
    let result = merge(dir.path(), &lane, "integ", false, &mut allow_all).unwrap();
    assert!(result.checkout_advanced, "{result:?}");
    assert_eq!(git(&sibling, &["status", "--porcelain"]), "");
    assert_eq!(staged(&sibling), "");
    assert!(sibling.join("y.txt").exists(), "the sibling did not follow");
    assert_eq!(symbolic_head(&sibling), "refs/heads/integ");
}

#[test]
fn a_dirty_sibling_checkout_of_the_target_refuses_and_is_untouched() {
    let (dir, _sibling_dir, sibling, _lane_dir, lane) = sibling_fixture();
    std::fs::write(sibling.join("y.txt"), "someone's work\n").unwrap();
    let before = git(dir.path(), &["rev-parse", "refs/heads/integ"]);
    let refused = merge(dir.path(), &lane, "integ", false, &mut allow_all).unwrap_err();
    let MergeTeardownError::Held {
        reason,
        path,
        message,
    } = refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!(reason, HeldReason::CheckoutDirty, "{message}");
    assert!(path.unwrap().ends_with("integ"), "{message}");
    assert_eq!(git(dir.path(), &["rev-parse", "refs/heads/integ"]), before);
    assert_eq!(symbolic_head(&sibling), "refs/heads/integ");
    assert_eq!(git(&sibling, &["status", "--porcelain"]), "?? y.txt");
    assert!(lane.exists() && branch_exists(dir.path(), "feature-y"));
}

#[test]
fn prepare_with_message_records_custom_commit_message() {
    let (dir, _lane) = lane_fixture();
    let repo = dir.path();
    let target_sha = git(repo, &["rev-parse", "refs/heads/main"]);
    let branch_sha = git(repo, &["rev-parse", "refs/heads/feature-x"]);
    let candidate = crate::engine::merge_tree::prepare_with_message(
        repo,
        "feature-x",
        "main",
        &target_sha,
        &branch_sha,
        true,
        Some("Custom squash landing message"),
        &mut allow_all,
    )
    .unwrap();
    assert_eq!(
        git(repo, &["log", "-1", "--format=%s", &candidate]),
        "Custom squash landing message"
    );
}

/// A sibling whose directory is gone cannot be shown clean, so it refuses.
#[test]
fn a_sibling_checkout_that_cannot_be_read_refuses() {
    let (dir, sibling_dir, sibling, _lane_dir, lane) = sibling_fixture();
    std::fs::remove_dir_all(&sibling).unwrap();
    let before = git(dir.path(), &["rev-parse", "refs/heads/integ"]);
    let refused = merge(dir.path(), &lane, "integ", false, &mut allow_all).unwrap_err();
    drop(sibling_dir);
    let MergeTeardownError::Held { reason, .. } = refused else {
        panic!("{refused:?}");
    };
    assert_eq!(reason, HeldReason::CheckoutUnreadable);
    assert_eq!(git(dir.path(), &["rev-parse", "refs/heads/integ"]), before);
    assert!(lane.exists() && branch_exists(dir.path(), "feature-y"));
}

fn write_hook(repo: &Path, name: &str, body: &str, executable: bool) {
    let hook = repo.join(".git/hooks").join(name);
    crate::test_support::write_git_script(&hook, body, executable);
}

/// `commit-tree` runs no hooks. The wave does not land a commit behind a
/// pre-commit hook (a secret scanner, say); it holds with a typed reason.
#[test]
fn the_wave_refuses_a_commit_that_would_skip_an_active_hook() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    write_hook(repo, "pre-commit", "#!/bin/sh\nexit 1\n", true);
    let before = git(repo, &["rev-parse", "refs/heads/main"]);
    let mut asked = Vec::new();
    let refused = merge_and_teardown_requiring(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        true,
        None,
        true,
        &mut |argv: &[&str]| {
            asked.push(argv.join(" "));
            Ok(())
        },
    )
    .unwrap_err();
    let MergeTeardownError::Held {
        reason, message, ..
    } = refused
    else {
        panic!("{refused:?}");
    };
    assert_eq!(reason, HeldReason::CommitHooks);
    assert!(message.contains("pre-commit"), "{message}");
    assert!(
        !asked
            .iter()
            .any(|a| a.starts_with("commit-tree") || a.starts_with("update-ref")),
        "{asked:?}"
    );
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(lane.path().exists() && branch_exists(repo, "feature-x"));
}

/// The hook check follows what git would run: a hook without the executable
/// bit is not active on Unix, and `core.hooksPath` replaces the default
/// directory on every platform.
#[test]
fn active_hooks_follow_the_executable_bit_and_hooks_path() {
    let (dir, _lane) = lane_fixture();
    let repo = dir.path();
    #[cfg(unix)]
    {
        write_hook(repo, "pre-commit", "#!/bin/sh\nexit 1\n", false);
        assert!(
            crate::engine::merge_tree::active_commit_hooks_with(repo, false, &mut allow_all)
                .unwrap()
                .is_empty()
        );
    }
    write_hook(repo, "commit-msg", "#!/bin/sh\nexit 1\n", true);
    assert_eq!(
        crate::engine::merge_tree::active_commit_hooks_with(repo, false, &mut allow_all).unwrap(),
        ["commit-msg"]
    );
    let elsewhere = tempfile::tempdir().unwrap();
    git(
        repo,
        &[
            "config",
            "core.hooksPath",
            &crate::test_support::sh_path(elsewhere.path()),
        ],
    );
    assert!(
        crate::engine::merge_tree::active_commit_hooks_with(repo, false, &mut allow_all)
            .unwrap()
            .is_empty()
    );
    crate::test_support::write_git_script(
        &elsewhere.path().join("pre-merge-commit"),
        "#!/bin/sh\nexit 0\n",
        true,
    );
    assert_eq!(
        crate::engine::merge_tree::active_commit_hooks_with(repo, false, &mut allow_all).unwrap(),
        ["pre-merge-commit"]
    );
}

/// A fast-forward records no commit, so there is nothing a hook would have
/// judged: an active hook does not stop it.
#[test]
fn a_fast_forward_lands_past_an_active_hook() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    write_hook(repo, "pre-commit", "#!/bin/sh\nexit 1\n", true);
    let result = merge_and_teardown_requiring(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        true,
        &mut allow_all,
    )
    .unwrap();
    assert!(result.merged && !result.checkout_fallback, "{result:?}");
}

/// The panel is not left without a way to merge: with an active hook it takes
/// the in-checkout merge, which runs the hook, and says so.
#[test]
fn the_panel_merges_in_the_checkout_when_a_hook_is_active() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    let marker = repo.join("hook-ran.marker");
    write_hook(
        repo,
        "pre-commit",
        &format!(
            "#!/bin/sh\necho ran > {}\nexit 0\n",
            crate::test_support::sh_quote(&marker)
        ),
        true,
    );
    let result = merge(repo, lane.path(), "main", true, &mut allow_all).unwrap();
    assert!(result.checkout_fallback, "{result:?}");
    let note = result.checkout_note.expect("the fallback is visible");
    assert!(note.contains("pre-commit"), "{note}");
    assert!(marker.exists(), "the hook did not run");
    assert!(repo.join("README.md").exists());
    std::fs::remove_file(&marker).ok();
}

/// And when that hook refuses the commit, the squash stays staged for the
/// person to finish or undo, the lane is kept, and nothing is torn down.
#[test]
fn a_hook_that_refuses_the_panels_squash_keeps_the_lane() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    write_hook(
        repo,
        "pre-commit",
        "#!/bin/sh\necho refusing this commit >&2\nexit 1\n",
        true,
    );
    let refused = merge(repo, lane.path(), "main", true, &mut allow_all)
        .unwrap_err()
        .to_string();
    assert!(
        refused.contains("could not be committed") && refused.contains("git reset --merge"),
        "{refused}"
    );
    assert!(lane.path().exists() && branch_exists(repo, "feature-x"));
    assert_eq!(git(repo, &["diff", "--cached", "--name-only"]), "README.md");
}

#[test]
fn verify_in_scratch_reuses_pooled_cow_worktree_and_preserves_target_and_node_modules() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let repo = dir.path();

    std::fs::write(repo.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
    git(repo, &["add", ".gitignore"]);
    git(repo, &["commit", "-q", "-m", "gitignore"]);
    let c1 = git(repo, &["rev-parse", "HEAD"]);

    // Seed target/ and node_modules/ in anchor repo so CoW seeding populates them.
    std::fs::create_dir_all(repo.join("target/debug")).unwrap();
    std::fs::write(repo.join("target/debug/seed.o"), "seed-obj").unwrap();
    std::fs::create_dir_all(repo.join("node_modules/pkg")).unwrap();
    std::fs::write(
        repo.join("node_modules/pkg/index.js"),
        "module.exports = 1;\n",
    )
    .unwrap();

    // Create a second commit c2.
    std::fs::write(repo.join("README.md"), "hello v2\n").unwrap();
    std::fs::write(repo.join("new_file.txt"), "from c2\n").unwrap();
    git(repo, &["add", "README.md", "new_file.txt"]);
    git(repo, &["commit", "-q", "-m", "c2"]);
    let c2 = git(repo, &["rev-parse", "HEAD"]);
    // Move main back to c1 so c2 is an unmerged candidate commit.
    git(repo, &["reset", "--hard", "-q", &c1]);

    let mut first_scratch = None;
    crate::engine::merge_tree::verify_in_scratch(
        repo,
        &c1,
        &mut |scratch, oid| {
            assert_eq!(oid, c1);
            assert!(
                scratch
                    .to_string_lossy()
                    .contains("gitpulse/verify-scratch/worktree"),
                "expected pooled scratch path, got {}",
                scratch.display()
            );
            assert!(scratch.join("target/debug/seed.o").exists());
            assert!(scratch.join("node_modules/pkg/index.js").exists());
            std::fs::write(scratch.join("target/debug/incremental.o"), "inc").unwrap();
            std::fs::write(scratch.join("node_modules/.cache.json"), "{}").unwrap();
            std::fs::write(scratch.join("stray_build.tmp"), "junk").unwrap();
            first_scratch = Some(scratch.to_path_buf());
            Ok(())
        },
        &mut allow_all,
    )
    .unwrap();

    let mut second_scratch = None;
    crate::engine::merge_tree::verify_in_scratch(
        repo,
        &c2,
        &mut |scratch, oid| {
            assert_eq!(oid, c2);
            second_scratch = Some(scratch.to_path_buf());
            assert!(
                scratch.join("target/debug/incremental.o").exists(),
                "target/ must be preserved across consecutive pooled runs"
            );
            assert!(
                scratch.join("node_modules/.cache.json").exists(),
                "node_modules/ must be preserved across consecutive pooled runs"
            );
            assert!(
                !scratch.join("stray_build.tmp").exists(),
                "stray untracked files must be cleaned"
            );
            assert_eq!(
                std::fs::read_to_string(scratch.join("new_file.txt")).unwrap(),
                "from c2\n"
            );
            assert_eq!(git(scratch, &["rev-parse", "HEAD"]), c2);
            Ok(())
        },
        &mut allow_all,
    )
    .unwrap();

    assert_eq!(first_scratch, second_scratch);
}

#[test]
fn verify_in_scratch_falls_back_to_temp_dir_when_pool_busy_or_low_disk() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path());
    let repo = dir.path();
    let head = git(repo, &["rev-parse", "HEAD"]);

    let common = crate::engine::git_cli::resolve_git_common_dir(repo).unwrap();
    let pool_root = common.join("gitpulse").join("verify-scratch");
    std::fs::create_dir_all(&pool_root).unwrap();
    let lock_file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(pool_root.join("pool.lock"))
        .unwrap();
    lock_file.try_lock().unwrap();

    let mut busy_path = None;
    crate::engine::merge_tree::verify_in_scratch(
        repo,
        &head,
        &mut |scratch, _| {
            busy_path = Some(scratch.to_path_buf());
            assert!(
                !scratch
                    .to_string_lossy()
                    .contains("gitpulse/verify-scratch/worktree"),
                "busy pool must fall back to detached temp_dir"
            );
            Ok(())
        },
        &mut allow_all,
    )
    .unwrap();
    assert!(
        !busy_path.unwrap().exists(),
        "temp_dir scratch must be removed"
    );
    lock_file.unlock().unwrap();

    let mut low_disk_path = None;
    crate::engine::merge_tree::verify_in_scratch_with_free_space(
        repo,
        &head,
        &mut |scratch, _| {
            low_disk_path = Some(scratch.to_path_buf());
            assert!(
                !scratch
                    .to_string_lossy()
                    .contains("gitpulse/verify-scratch/worktree"),
                "low disk must fall back to detached temp_dir"
            );
            Ok(())
        },
        &mut allow_all,
        |_| crate::sysload::Reading::Value(crate::sysload::LOW_DISK_FLOOR - 1),
    )
    .unwrap();
    assert!(
        !low_disk_path.unwrap().exists(),
        "low-disk temp_dir scratch must be removed"
    );
}

#[test]
fn land_advances_green_ref_after_verify_in_scratch_and_stays_nonfatal_on_rewind() {
    let (dir, lane) = lane_fixture();
    let repo = dir.path();
    let target_sha = git(repo, &["rev-parse", "refs/heads/main"]);
    let branch_sha = git(repo, &["rev-parse", "refs/heads/feature-x"]);

    let candidate = crate::engine::merge_tree::prepare(
        repo,
        "feature-x",
        "main",
        &target_sha,
        &branch_sha,
        false,
        &mut allow_all,
    )
    .unwrap();
    crate::engine::merge_tree::verify_in_scratch(
        repo,
        &candidate,
        &mut |scratch, _| {
            assert_eq!(git(scratch, &["rev-parse", "HEAD"]), candidate);
            Ok(())
        },
        &mut allow_all,
    )
    .unwrap();
    let touch = crate::engine::merge_tree::checkout_holders_for_target(repo, "main").unwrap();
    let landed = crate::engine::merge_tree::land(
        repo,
        "feature-x",
        "main",
        &target_sha,
        &candidate,
        &touch,
        &mut allow_all,
    )
    .unwrap();
    assert!(
        landed.green_ref_note.is_none(),
        "{:?}",
        landed.green_ref_note
    );
    assert_eq!(
        git(repo, &["rev-parse", "--verify", "refs/gitpulse/green/main"]),
        landed.candidate_oid
    );

    // Create a diverged commit on refs/gitpulse/green/main so advance_green_ref refuses rewind,
    // and verify a subsequent verified land() still succeeds with an advisory note.
    let diverged_tree = git(repo, &["rev-parse", &format!("{target_sha}^{{tree}}")]);
    let diverged_commit = git(
        repo,
        &["commit-tree", &diverged_tree, "-m", "diverged green"],
    );
    git(
        repo,
        &["update-ref", "refs/gitpulse/green/main", &diverged_commit],
    );

    std::fs::write(lane.path().join("second.txt"), "second\n").unwrap();
    git(lane.path(), &["add", "second.txt"]);
    git(lane.path(), &["commit", "-q", "-m", "second commit"]);
    let target_sha2 = git(repo, &["rev-parse", "refs/heads/main"]);
    let branch_sha2 = git(repo, &["rev-parse", "refs/heads/feature-x"]);
    let candidate2 = crate::engine::merge_tree::prepare(
        repo,
        "feature-x",
        "main",
        &target_sha2,
        &branch_sha2,
        false,
        &mut allow_all,
    )
    .unwrap();
    crate::engine::merge_tree::verify_in_scratch(
        repo,
        &candidate2,
        &mut |_, _| Ok(()),
        &mut allow_all,
    )
    .unwrap();
    let touch2 = crate::engine::merge_tree::checkout_holders_for_target(repo, "main").unwrap();
    let landed2 = crate::engine::merge_tree::land(
        repo,
        "feature-x",
        "main",
        &target_sha2,
        &candidate2,
        &touch2,
        &mut allow_all,
    )
    .unwrap();
    assert_eq!(
        git(repo, &["rev-parse", "refs/heads/main"]),
        landed2.candidate_oid
    );
    assert!(
        landed2
            .green_ref_note
            .as_deref()
            .is_some_and(|n| n.contains("advisory:")),
        "expected advisory green_ref_note, got {:?}",
        landed2.green_ref_note
    );
    // Green ref was not rewound/clobbered.
    assert_eq!(
        git(repo, &["rev-parse", "--verify", "refs/gitpulse/green/main"]),
        diverged_commit
    );
}
