//! Adversarial tests for lane integration in the object database: dirt that
//! appears after the cleanliness check, a revert that must not overwrite
//! another actor's ref move, restores that must be exact for every holder, and
//! the commit hooks git would run (files, `core.hooksPath`, symlinks, config
//! hooks and `prepare-commit-msg`).

use crate::engine::git_cli::git_text;
#[cfg(unix)]
use crate::engine::merge_tree::HeldReason;
use crate::engine::merge_tree::{active_commit_hooks_with, classify_hook_list};
use crate::engine::worktree::{add_worktree_extended, merge_and_teardown_worktree};
#[cfg(unix)]
use crate::engine::worktree::{merge_and_teardown_requiring, MergeTeardownError};
use crate::ipc_error::CommandError;
use std::collections::BTreeMap;
use std::path::Path;
#[cfg(unix)]
use std::path::PathBuf;

fn git(cwd: &Path, args: &[&str]) -> String {
    git_text(cwd, args)
        .unwrap_or_else(|e| panic!("git {args:?} in {}: {}", cwd.display(), e.message()))
        .trim()
        .to_string()
}

fn init_repo(dir: &Path, gitignore: Option<&str>) {
    crate::test_support::git_in(dir, &["init", "-q", "-b", "main", "."]);
    git(dir, &["config", "user.email", "test@gitpulse.local"]);
    git(dir, &["config", "user.name", "GitPulse Tester"]);
    std::fs::write(dir.join("README.md"), "hello\n").unwrap();
    std::fs::write(dir.join("other.txt"), "other\n").unwrap();
    if let Some(ignore) = gitignore {
        std::fs::write(dir.join(".gitignore"), ignore).unwrap();
    }
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", "initial"]);
    crate::test_support::trust_repo(dir);
}

/// Lane edits README.md, adds new.txt and force-adds gen.out (ignored on main).
fn fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), Some("gen.out\n"));
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
    std::fs::write(lane.path().join("new.txt"), "lane new\n").unwrap();
    std::fs::write(lane.path().join("gen.out"), "lane generated\n").unwrap();
    git(lane.path(), &["add", "README.md", "new.txt"]);
    git(lane.path(), &["add", "-f", "gen.out"]);
    git(lane.path(), &["commit", "-q", "-m", "lane"]);
    (dir, lane)
}

fn walk(root: &Path, dir: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            walk(root, &path, out);
        } else {
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .to_string();
            out.insert(rel, std::fs::read(&path).unwrap());
        }
    }
}

/// HEAD (symbolic and oid), the index (`ls-files -s`), and every file's bytes.
#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    symbolic: String,
    head: String,
    index: String,
    files: BTreeMap<String, Vec<u8>>,
}

fn snapshot(path: &Path) -> Snapshot {
    let mut files = BTreeMap::new();
    walk(path, path, &mut files);
    Snapshot {
        symbolic: git_text(path, &["symbolic-ref", "-q", "HEAD"])
            .map(|s| s.trim().to_string())
            .unwrap_or_default(),
        head: git(path, &["rev-parse", "HEAD"]),
        index: git(path, &["ls-files", "-s"]),
        files,
    }
}

/// The holder becomes dirty after the cleanliness check, inside the window
/// between the CAS and `read-tree`. Uncommitted work must survive byte for
/// byte, and a refusal must leave HEAD, index and files exactly as the person
/// left them.
#[test]
fn dirt_after_the_check_is_never_clobbered() {
    type Dirt = fn(&Path);
    let cases: [(&str, Dirt, bool); 4] = [
        (
            "modified tracked file the merge changes",
            |r| std::fs::write(r.join("README.md"), "user edit\n").unwrap(),
            false,
        ),
        (
            "untracked file at a path the candidate adds",
            |r| std::fs::write(r.join("new.txt"), "user untracked\n").unwrap(),
            false,
        ),
        // An ignored file at a path the candidate adds is overwritten, as
        // `git merge --ff-only` and `git checkout` also do (git treats ignored
        // files as expendable); shown by a raw-git repro, not asserted here.
        (
            "staged change to a file the merge changes",
            |r| {
                std::fs::write(r.join("README.md"), "user staged\n").unwrap();
                crate::test_support::git_in(r, &["add", "README.md"]);
            },
            false,
        ),
        (
            "modified tracked file the merge does not touch",
            |r| std::fs::write(r.join("other.txt"), "user edit other\n").unwrap(),
            true,
        ),
    ];
    for (label, dirt, may_land) in cases {
        let (dir, lane) = fixture();
        let repo = dir.path().to_path_buf();
        let before_ref = git(&repo, &["rev-parse", "refs/heads/main"]);
        let mut injected: Option<Snapshot> = None;
        let result = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                if argv.first() == Some(&"update-index") && injected.is_none() {
                    dirt(&repo);
                    injected = Some(snapshot(&repo));
                }
                Ok(())
            },
        );
        let injected = injected.unwrap_or_else(|| panic!("{label}: the window was never reached"));
        let after = snapshot(&repo);
        eprintln!(
            "{label}: {:?}",
            result
                .as_ref()
                .map(|r| (&r.checkout_advanced, &r.checkout_note))
        );
        match result {
            Err(error) => {
                assert_eq!(
                    git(&repo, &["rev-parse", "refs/heads/main"]),
                    before_ref,
                    "{label}: the ref was not put back: {error:?}"
                );
                assert_eq!(after.symbolic, "refs/heads/main", "{label}");
                assert_eq!(after.head, before_ref, "{label}: HEAD not back: {error:?}");
                assert_eq!(
                    after.index, injected.index,
                    "{label}: the index changed: {error:?}"
                );
                assert_eq!(
                    after.files, injected.files,
                    "{label}: files changed: {error:?}"
                );
                assert!(lane.path().exists(), "{label}: the lane was removed");
            }
            Ok(done) => {
                assert!(may_land, "{label}: landed over the person's work: {done:?}");
                let other = std::fs::read(repo.join("other.txt")).unwrap();
                assert_eq!(other, b"user edit other\n", "{label}");
                assert_eq!(
                    git(&repo, &["diff", "--cached", "--name-only"]),
                    "",
                    "{label}"
                );
                assert_eq!(
                    git(&repo, &["rev-parse", "HEAD"]),
                    done.after_oid.clone().unwrap()
                );
            }
        }
    }
}

/// The forward `read-tree` fails and, before the revert, someone else moves
/// the ref past the candidate. The revert (CAS with the candidate as the
/// expected old value) must refuse, leave that move, and say so.
#[test]
fn the_revert_never_overwrites_another_actors_ref_move() {
    let (dir, lane) = fixture();
    let repo = dir.path().to_path_buf();
    let mut theirs = String::new();
    let mut failed_once = false;
    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |argv: &[&str]| {
            if argv.first() == Some(&"read-tree") && !failed_once {
                failed_once = true;
                let candidate = git(&repo, &["rev-parse", "refs/heads/main"]);
                let tree = format!("{candidate}^{{tree}}");
                theirs = git(
                    &repo,
                    &[
                        "commit-tree",
                        tree.as_str(),
                        "-p",
                        candidate.as_str(),
                        "-m",
                        "theirs",
                    ],
                );
                git(
                    &repo,
                    &[
                        "update-ref",
                        "refs/heads/main",
                        theirs.as_str(),
                        candidate.as_str(),
                    ],
                );
                return Err(CommandError::plain("injected read-tree failure"));
            }
            Ok(())
        },
    )
    .unwrap_err();
    let message = refused.message().to_string();
    eprintln!("{refused:?}");
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/main"]),
        theirs,
        "{message}"
    );
    assert!(message.contains("restore was incomplete"), "{message}");
    assert!(message.contains("could not be put back"), "{message}");
    assert!(lane.path().exists());
}

/// Two checkouts hold main (the main checkout and a forced second one). The
/// first follows, the second cannot, and the first cannot be moved back
/// because the person edited it meanwhile. Edits survive in both, and the
/// report names the first checkout as not restored.
#[test]
fn a_holder_that_cannot_be_moved_back_is_reported_and_its_work_kept() {
    let (dir, lane) = fixture();
    let repo = dir.path().to_path_buf();
    let second_dir = tempfile::tempdir().unwrap();
    let second = second_dir.path().join("second");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-f",
            second.to_str().unwrap(),
            "main",
        ],
    );
    crate::test_support::trust_repo(&second);
    let before_ref = git(&repo, &["rev-parse", "refs/heads/main"]);
    let mut refreshes = 0;
    let result = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |argv: &[&str]| {
            if argv.first() == Some(&"update-index") {
                refreshes += 1;
                if refreshes == 2 {
                    // The first holder has followed; edit both now.
                    std::fs::write(repo.join("README.md"), "first holder edit\n").unwrap();
                    std::fs::write(second.join("README.md"), "second holder edit\n").unwrap();
                }
            }
            Ok(())
        },
    );
    eprintln!("{result:?}");
    eprintln!("first: {:?}", git(&repo, &["status", "--porcelain"]));
    eprintln!("second: {:?}", git(&second, &["status", "--porcelain"]));
    let refused = result.expect_err("the second holder could not follow");
    let message = refused.message().to_string();
    assert_eq!(
        std::fs::read_to_string(repo.join("README.md")).unwrap(),
        "first holder edit\n"
    );
    assert_eq!(
        std::fs::read_to_string(second.join("README.md")).unwrap(),
        "second holder edit\n"
    );
    assert_eq!(
        git(&repo, &["rev-parse", "refs/heads/main"]),
        before_ref,
        "{message}"
    );
    assert!(message.contains("restore was incomplete"), "{message}");
    assert!(message.contains("could not be moved back"), "{message}");
    assert!(lane.path().exists());
}

/// Clean holders, the advance fails on the second: after the restore every
/// holder is byte-identical to before (HEAD, index, files).
#[test]
fn a_restore_is_byte_identical_for_every_holder() {
    let (dir, lane) = fixture();
    let repo = dir.path().to_path_buf();
    let second_dir = tempfile::tempdir().unwrap();
    let second = second_dir.path().join("second");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "-f",
            second.to_str().unwrap(),
            "main",
        ],
    );
    crate::test_support::trust_repo(&second);
    let first_before = snapshot(&repo);
    let second_before = snapshot(&second);
    let mut reads = 0;
    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |argv: &[&str]| {
            if argv.first() == Some(&"read-tree") {
                reads += 1;
                if reads == 2 {
                    return Err(CommandError::plain("injected: second holder cannot follow"));
                }
            }
            Ok(())
        },
    )
    .unwrap_err();
    eprintln!("{refused:?}");
    assert_eq!(snapshot(&repo), first_before, "first holder");
    assert_eq!(snapshot(&second), second_before, "second holder");
    assert!(lane.path().exists());
}

fn diverged_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
    let (dir, lane) = fixture();
    std::fs::write(dir.path().join("main-only.txt"), "main\n").unwrap();
    git(dir.path(), &["add", "main-only.txt"]);
    git(dir.path(), &["commit", "-q", "-m", "main moves"]);
    (dir, lane)
}

#[cfg(unix)]
fn wave(dir: &Path, lane: &Path) -> Result<(), MergeTeardownError> {
    merge_and_teardown_requiring(
        dir.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("main"),
        false,
        None,
        true,
        &mut |_: &[&str]| Ok(()),
    )
    .map(drop)
}

#[cfg(unix)]
fn expect_hooks_held(label: &str, outcome: Result<(), MergeTeardownError>) {
    match outcome {
        Err(MergeTeardownError::Held {
            reason: HeldReason::CommitHooks,
            ..
        }) => {}
        other => BYPASSED.with(|b| b.borrow_mut().push(format!("{label}: {other:?}"))),
    }
}

thread_local! {
    static BYPASSED: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

#[cfg(unix)]
fn exec_file(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Hook configurations git 2.54 honours on `git commit`/`git merge`.
#[cfg(unix)]
#[test]
fn hooks_git_would_run_are_never_skipped() {
    // Relative core.hooksPath.
    {
        let (dir, lane) = diverged_fixture();
        git(dir.path(), &["config", "core.hooksPath", "githooks"]);
        exec_file(
            &dir.path().join("githooks/pre-commit"),
            "#!/bin/sh\nexit 1\n",
        );
        expect_hooks_held("relative core.hooksPath", wave(dir.path(), lane.path()));
    }
    // Per-worktree config (extensions.worktreeConfig).
    {
        let (dir, lane) = diverged_fixture();
        let hooks = tempfile::tempdir().unwrap();
        git(dir.path(), &["config", "extensions.worktreeConfig", "true"]);
        git(
            dir.path(),
            &[
                "config",
                "--worktree",
                "core.hooksPath",
                hooks.path().to_str().unwrap(),
            ],
        );
        exec_file(&hooks.path().join("commit-msg"), "#!/bin/sh\nexit 1\n");
        expect_hooks_held("per-worktree core.hooksPath", wave(dir.path(), lane.path()));
    }
    // A hook that is a symlink to an executable script.
    {
        let (dir, lane) = diverged_fixture();
        let real = tempfile::tempdir().unwrap();
        exec_file(&real.path().join("scan"), "#!/bin/sh\nexit 1\n");
        std::os::unix::fs::symlink(
            real.path().join("scan"),
            dir.path().join(".git/hooks/pre-merge-commit"),
        )
        .unwrap();
        expect_hooks_held("symlinked hook", wave(dir.path(), lane.path()));
    }
    // Config-defined hook (hook.<name>.event), no file in any hooks dir.
    {
        let (dir, lane) = diverged_fixture();
        git(dir.path(), &["config", "hook.scan.command", "exit 1"]);
        git(dir.path(), &["config", "hook.scan.event", "pre-commit"]);
        expect_hooks_held(
            "config-defined hook.scan.event",
            wave(dir.path(), lane.path()),
        );
    }
    // prepare-commit-msg: git commit/merge run it and abort on non-zero.
    {
        let (dir, lane) = diverged_fixture();
        exec_file(
            &dir.path().join(".git/hooks/prepare-commit-msg"),
            "#!/bin/sh\nexit 1\n",
        );
        expect_hooks_held("prepare-commit-msg", wave(dir.path(), lane.path()));
    }
    let bypassed = BYPASSED.with(|b| b.borrow().clone());
    assert!(
        bypassed.is_empty(),
        "hooks git would run were skipped: {bypassed:?}"
    );
}

/// Config-defined hooks exist only from git 2.54; a git that cannot run
/// `hook list` has none, so the directory is its whole answer. With `hook list`
/// the config hook is found, and the directory scan still finds files either way.
#[cfg(unix)]
#[test]
fn hook_list_support_decides_whether_config_hooks_are_looked_for() {
    let (dir, _lane) = diverged_fixture();
    let repo = dir.path();
    git(repo, &["config", "hook.scan.command", "exit 1"]);
    git(repo, &["config", "hook.scan.event", "pre-commit"]);
    let asked = std::cell::RefCell::new(Vec::<String>::new());
    let mut gate = |argv: &[&str]| {
        asked.borrow_mut().push(argv.join(" "));
        Ok(())
    };
    assert!(active_commit_hooks_with(repo, false, &mut gate)
        .unwrap()
        .is_empty());
    assert_eq!(
        active_commit_hooks_with(repo, true, &mut gate).unwrap(),
        ["pre-commit"]
    );
    assert!(
        asked.borrow().iter().any(|a| a == "hook list pre-commit"),
        "{:?}",
        asked.borrow()
    );
    exec_file(&repo.join(".git/hooks/commit-msg"), "#!/bin/sh\nexit 1\n");
    assert_eq!(
        active_commit_hooks_with(repo, false, &mut gate).unwrap(),
        ["commit-msg"]
    );
}

/// `git hook list` that is refused by the gate refuses the integration instead
/// of reading as "no hooks".
#[test]
fn a_gate_that_refuses_hook_list_refuses_the_check() {
    let (dir, _lane) = diverged_fixture();
    let refused = active_commit_hooks_with(dir.path(), true, &mut |argv: &[&str]| {
        if argv.first() == Some(&"hook") {
            Err(CommandError::plain("the gate refused hook list"))
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(
        refused.message().contains("refused"),
        "{}",
        refused.message()
    );
}

/// What `hook list` said: only exit 1 with "no hooks found" is none.
#[test]
fn only_an_exact_no_hooks_found_answer_is_no_hooks() {
    assert_eq!(
        classify_hook_list("pre-commit", 0, ""),
        Some("pre-commit".into())
    );
    assert_eq!(
        classify_hook_list(
            "pre-commit",
            1,
            "warning: no hooks found for event 'pre-commit'"
        ),
        None
    );
    for (status, stderr) in [
        (1, "fatal: bad config"),
        (2, "no hooks found"),
        (128, ""),
        (-1, ""),
    ] {
        let said = classify_hook_list("commit-msg", status, stderr);
        assert!(
            said.as_deref()
                .is_some_and(|text| text.starts_with("commit-msg (git hook list exited")),
            "{status} {stderr}: {said:?}"
        );
    }
}

/// A hooks directory that cannot be read must not read as "no hooks".
#[cfg(unix)]
#[test]
fn an_unreadable_hooks_dir_fails_closed() {
    use std::os::unix::fs::PermissionsExt;
    let (dir, lane) = diverged_fixture();
    let hooks = dir.path().join(".git/hooks");
    std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o000)).unwrap();
    let before = git(dir.path(), &["rev-parse", "refs/heads/main"]);
    let outcome = wave(dir.path(), lane.path());
    std::fs::set_permissions(&hooks, std::fs::Permissions::from_mode(0o755)).unwrap();
    eprintln!("{outcome:?}");
    assert!(outcome.is_err(), "landed with an unreadable hooks dir");
    assert_eq!(git(dir.path(), &["rev-parse", "refs/heads/main"]), before);
    let _ = PathBuf::new();
}

/// M2 Test 1: Disjoint staged, unstaged, and untracked edits in the target
/// checkout survive `land()` intact while incoming changes materialize.
#[test]
fn disjoint_dirty_staged_unstaged_and_untracked_survive_land() {
    for squash in [false, true] {
        let (dir, lane) = fixture();
        let repo = dir.path();
        std::fs::write(repo.join("tracked_staged.txt"), "base\n").unwrap();
        git(repo, &["add", "tracked_staged.txt"]);
        git(repo, &["commit", "-q", "-m", "add tracked_staged"]);

        // Disjoint staged add, staged modification, unstaged modification, and untracked file.
        std::fs::write(repo.join("staged_new.txt"), "user staged new\n").unwrap();
        git(repo, &["add", "staged_new.txt"]);
        std::fs::write(repo.join("tracked_staged.txt"), "user staged mod\n").unwrap();
        git(repo, &["add", "tracked_staged.txt"]);
        std::fs::write(repo.join("other.txt"), "user unstaged mod\n").unwrap();
        std::fs::write(repo.join("untracked.txt"), "user untracked\n").unwrap();

        let done = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("main"),
            squash,
            None,
            &mut |_| Ok(()),
        )
        .unwrap_or_else(|e| panic!("squash={squash}: {e}"));

        assert!(
            done.merged && done.checkout_advanced,
            "squash={squash}: {done:?}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("README.md")).unwrap(),
            "hello\nlane line\n",
            "squash={squash}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "lane new\n",
            "squash={squash}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("staged_new.txt")).unwrap(),
            "user staged new\n",
            "squash={squash}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("tracked_staged.txt")).unwrap(),
            "user staged mod\n",
            "squash={squash}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("other.txt")).unwrap(),
            "user unstaged mod\n",
            "squash={squash}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("untracked.txt")).unwrap(),
            "user untracked\n",
            "squash={squash}"
        );
        let status = git_text(repo, &["status", "--porcelain"]).unwrap();
        assert!(
            status.contains("A  staged_new.txt"),
            "squash={squash}: {status}"
        );
        assert!(
            status.contains("M  tracked_staged.txt"),
            "squash={squash}: {status}"
        );
        assert!(status.contains(" M other.txt"), "squash={squash}: {status}");
        assert!(
            status.contains("?? untracked.txt"),
            "squash={squash}: {status}"
        );
        assert_eq!(
            git(repo, &["symbolic-ref", "-q", "HEAD"]),
            "refs/heads/main"
        );
        assert_eq!(git(repo, &["rev-parse", "HEAD"]), done.after_oid.unwrap());
    }
}

/// M2 Test 2: APFS / case-insensitive collision (staged `foo.txt` vs incoming
/// `Foo.txt`) and non-ASCII paths on case-insensitive filesystems refuse with
/// `HeldReason::CheckoutDirty` and zero bytes overwritten.
#[cfg(unix)]
#[test]
fn apfs_case_collision_with_staged_file_refuses_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), None);
    let repo = dir.path();
    git(repo, &["config", "core.ignorecase", "true"]);
    let lane = tempfile::tempdir().unwrap();
    add_worktree_extended(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("feature-x"),
        None,
        false,
        false,
        false,
    )
    .unwrap();
    crate::test_support::trust_repo(lane.path());
    std::fs::write(lane.path().join("Foo.txt"), "incoming Foo\n").unwrap();
    git(lane.path(), &["add", "Foo.txt"]);
    git(lane.path(), &["commit", "-q", "-m", "add Foo.txt"]);

    std::fs::write(repo.join("foo.txt"), "precious staged foo\n").unwrap();
    git(repo, &["add", "foo.txt"]);
    let before = git(repo, &["rev-parse", "refs/heads/main"]);

    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    let MergeTeardownError::Held {
        reason: HeldReason::CheckoutDirty,
        ..
    } = refused
    else {
        panic!("expected CheckoutDirty, got {refused:?}");
    };
    assert_eq!(
        std::fs::read(repo.join("foo.txt")).unwrap(),
        b"precious staged foo\n"
    );
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(lane.path().exists());

    // Non-ASCII dirty path on case-insensitive filesystem also fails closed.
    let (dir2, lane2) = fixture();
    let repo2 = dir2.path();
    git(repo2, &["config", "core.ignorecase", "true"]);
    std::fs::write(repo2.join("caf\u{00e9}.txt"), "user non-ascii\n").unwrap();
    let before2 = git(repo2, &["rev-parse", "refs/heads/main"]);
    let refused2 = merge_and_teardown_worktree(
        repo2.to_str().unwrap(),
        lane2.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    assert!(
        matches!(
            refused2,
            MergeTeardownError::Held {
                reason: HeldReason::CheckoutDirty,
                ..
            }
        ),
        "{refused2:?}"
    );
    assert_eq!(git(repo2, &["rev-parse", "refs/heads/main"]), before2);
}

/// M2 Test 3: Gitignored local `ign.log` collides with incoming `ign.log` ->
/// refuses with `HeldReason::CheckoutDirty`, zero bytes overwritten.
#[cfg(unix)]
#[test]
fn gitignored_file_collision_refuses_without_overwriting() {
    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), Some("ign.log\n"));
    let repo = dir.path();
    let lane = tempfile::tempdir().unwrap();
    add_worktree_extended(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("feature-x"),
        None,
        false,
        false,
        false,
    )
    .unwrap();
    crate::test_support::trust_repo(lane.path());
    std::fs::write(lane.path().join("ign.log"), "incoming ignored\n").unwrap();
    git(lane.path(), &["add", "-f", "ign.log"]);
    git(lane.path(), &["commit", "-q", "-m", "force add ign.log"]);

    std::fs::write(repo.join("ign.log"), "precious local log\n").unwrap();
    assert_eq!(git(repo, &["status", "--porcelain"]), "");
    let before = git(repo, &["rev-parse", "refs/heads/main"]);

    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    let MergeTeardownError::Held {
        reason: HeldReason::CheckoutDirty,
        ..
    } = refused
    else {
        panic!("expected CheckoutDirty, got {refused:?}");
    };
    assert_eq!(
        std::fs::read(repo.join("ign.log")).unwrap(),
        b"precious local log\n"
    );
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(lane.path().exists());
}

/// M2 Test 4: Incoming `160000` gitlink/submodule entry refuses disjoint-dirty
/// advancement with `HeldReason::CheckoutDirty`.
#[cfg(unix)]
#[test]
fn submodule_gitlink_advance_on_dirty_holder_refuses() {
    let sub_dir = tempfile::tempdir().unwrap();
    init_repo(sub_dir.path(), None);
    let c1 = git(sub_dir.path(), &["rev-parse", "HEAD"]);
    std::fs::write(sub_dir.path().join("README.md"), "sub v2\n").unwrap();
    git(sub_dir.path(), &["commit", "-q", "-am", "sub v2"]);
    let c2 = git(sub_dir.path(), &["rev-parse", "HEAD"]);

    let dir = tempfile::tempdir().unwrap();
    init_repo(dir.path(), None);
    let repo = dir.path();
    git(
        repo,
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{c1},sub"),
        ],
    );
    git(repo, &["commit", "-q", "-m", "add gitlink"]);

    let lane = tempfile::tempdir().unwrap();
    add_worktree_extended(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("feature-x"),
        None,
        false,
        false,
        false,
    )
    .unwrap();
    crate::test_support::trust_repo(lane.path());
    git(
        lane.path(),
        &[
            "update-index",
            "--add",
            "--cacheinfo",
            &format!("160000,{c2},sub"),
        ],
    );
    git(lane.path(), &["commit", "-q", "-m", "advance gitlink"]);

    std::fs::write(repo.join("unrelated.txt"), "user wip\n").unwrap();
    let before = git(repo, &["rev-parse", "refs/heads/main"]);

    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    let MergeTeardownError::Held {
        reason: HeldReason::CheckoutDirty,
        ..
    } = refused
    else {
        panic!("expected CheckoutDirty, got {refused:?}");
    };
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert_eq!(
        std::fs::read_to_string(repo.join("unrelated.txt")).unwrap(),
        "user wip\n"
    );
    assert!(lane.path().exists());
}

/// M2 Test 5: Truncated dirty scan (`> MAX_CHANGED_PATHS = 2048`) refuses
/// disjoint-dirty advancement with `HeldReason::CheckoutDirty`.
#[cfg(unix)]
#[test]
fn truncated_dirty_scan_refuses_disjoint_advance() {
    let (dir, lane) = fixture();
    let repo = dir.path();
    git(repo, &["config", "status.showUntrackedFiles", "all"]);
    let bulk = repo.join("bulk");
    std::fs::create_dir_all(&bulk).unwrap();
    for i in 0..2060 {
        std::fs::write(bulk.join(format!("f_{i:04}.txt")), "").unwrap();
    }
    let before = git(repo, &["rev-parse", "refs/heads/main"]);

    let refused = merge_and_teardown_worktree(
        repo.to_str().unwrap(),
        lane.path().to_str().unwrap(),
        Some("main"),
        false,
        None,
        &mut |_| Ok(()),
    )
    .unwrap_err();
    let MergeTeardownError::Held {
        reason: HeldReason::CheckoutDirty,
        ..
    } = refused
    else {
        panic!("expected CheckoutDirty, got {refused:?}");
    };
    assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    assert!(lane.path().exists());
}

/// M2 Test 6: Rollback with pre-existing staged edits preserves staged edits,
/// restores incoming files, and passes `pre_cached_digest` verification
/// without emitting a false "restore was incomplete".
#[test]
fn rollback_with_preexisting_staged_edits_preserves_edits_and_passes_digest_check() {
    // Part A: single holder read-tree fails with pre-existing staged + unstaged edits.
    {
        let (dir, lane) = fixture();
        let repo = dir.path();
        std::fs::write(repo.join("staged.txt"), "precious staged\n").unwrap();
        git(repo, &["add", "staged.txt"]);
        std::fs::write(repo.join("other.txt"), "staged mod\n").unwrap();
        git(repo, &["add", "other.txt"]);
        std::fs::write(repo.join("unstaged.txt"), "precious unstaged\n").unwrap();
        let before = git(repo, &["rev-parse", "refs/heads/main"]);

        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                if argv.first() == Some(&"read-tree") {
                    return Err(CommandError::plain("injected read-tree failure"));
                }
                Ok(())
            },
        )
        .unwrap_err();
        let msg = refused.message().to_string();
        assert!(
            !msg.contains("restore was incomplete") && !msg.contains("does not match"),
            "unexpected false incomplete-restore message: {msg}"
        );
        assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
        assert_eq!(
            std::fs::read_to_string(repo.join("staged.txt")).unwrap(),
            "precious staged\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("other.txt")).unwrap(),
            "staged mod\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("unstaged.txt")).unwrap(),
            "precious unstaged\n"
        );
        let cached = git(repo, &["diff", "--cached", "--name-status"]);
        assert!(cached.contains("M\tother.txt"), "{cached}");
        assert!(cached.contains("A\tstaged.txt"), "{cached}");
        assert!(lane.path().exists());
    }

    // Part B: two holders; first holder has pre-existing staged edit, advances
    // cleanly, second holder fails `read-tree`, first holder rolls back cleanly.
    {
        let (dir, lane) = fixture();
        let repo = dir.path();
        let second_dir = tempfile::tempdir().unwrap();
        let second = second_dir.path().join("second");
        git(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-f",
                second.to_str().unwrap(),
                "main",
            ],
        );
        crate::test_support::trust_repo(&second);

        std::fs::write(repo.join("staged.txt"), "keep staged\n").unwrap();
        git(repo, &["add", "staged.txt"]);
        let before = git(repo, &["rev-parse", "refs/heads/main"]);
        let first_before = snapshot(repo);
        let second_before = snapshot(&second);

        let mut reads = 0;
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |argv: &[&str]| {
                if argv.first() == Some(&"read-tree") {
                    reads += 1;
                    if reads == 2 {
                        return Err(CommandError::plain("injected second holder failure"));
                    }
                }
                Ok(())
            },
        )
        .unwrap_err();
        let msg = refused.message().to_string();
        assert!(
            !msg.contains("restore was incomplete"),
            "unexpected incomplete restore: {msg}"
        );
        assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
        assert_eq!(snapshot(repo), first_before);
        assert_eq!(snapshot(&second), second_before);
        assert!(lane.path().exists());
    }
}

/// Trap E + directory prefix ancestry: parked operation on a non-anchor linked
/// holder and directory-prefix collision (`sub` vs `sub/nested.txt`) both refuse
/// with `HeldReason::CheckoutDirty`.
#[cfg(unix)]
#[test]
fn parked_op_on_linked_holder_and_prefix_ancestry_both_refuse() {
    // Parked MERGE_HEAD on linked holder of `integ`.
    {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), None);
        let repo = dir.path();
        let sibling_dir = tempfile::tempdir().unwrap();
        let sibling = sibling_dir.path().join("integ");
        git(
            repo,
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
            repo,
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

        let head_oid = git(&sibling, &["rev-parse", "HEAD"]);
        let sibling_git_dir = crate::engine::git_cli::resolve_git_dir(&sibling).unwrap();
        std::fs::write(sibling_git_dir.join("MERGE_HEAD"), format!("{head_oid}\n")).unwrap();
        let before = git(repo, &["rev-parse", "refs/heads/integ"]);

        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.to_str().unwrap(),
            Some("integ"),
            false,
            None,
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert!(
            matches!(
                refused,
                MergeTeardownError::Held {
                    reason: HeldReason::CheckoutDirty,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(git(repo, &["rev-parse", "refs/heads/integ"]), before);
    }

    // Directory prefix ancestry: untracked `sub` vs incoming `sub/nested.txt`.
    {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path(), None);
        let repo = dir.path();
        let lane = tempfile::tempdir().unwrap();
        add_worktree_extended(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("feature-x"),
            None,
            false,
            false,
            false,
        )
        .unwrap();
        crate::test_support::trust_repo(lane.path());
        std::fs::create_dir_all(lane.path().join("sub")).unwrap();
        std::fs::write(lane.path().join("sub/nested.txt"), "nested\n").unwrap();
        git(lane.path(), &["add", "sub/nested.txt"]);
        git(lane.path(), &["commit", "-q", "-m", "add sub/nested.txt"]);

        std::fs::write(repo.join("sub"), "user file at sub\n").unwrap();
        let before = git(repo, &["rev-parse", "refs/heads/main"]);
        let refused = merge_and_teardown_worktree(
            repo.to_str().unwrap(),
            lane.path().to_str().unwrap(),
            Some("main"),
            false,
            None,
            &mut |_| Ok(()),
        )
        .unwrap_err();
        assert!(
            matches!(
                refused,
                MergeTeardownError::Held {
                    reason: HeldReason::CheckoutDirty,
                    ..
                }
            ),
            "{refused:?}"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("sub")).unwrap(),
            "user file at sub\n"
        );
        assert_eq!(git(repo, &["rev-parse", "refs/heads/main"]), before);
    }
}
