//! Adversarial review of the shared lock-retry, unborn-HEAD and `RunError`
//! classifiers (task ft-b4eb6688). Tests marked `#[ignore = "break: ..."]` are
//! known regressions or wrong answers in the implementation under review; they
//! assert the CORRECT behaviour and fail today.

use super::*;
use crate::engine::git_reader::{head_is_unborn, GitReader};
use crate::procguard::LockedSpawn;
use crate::test_support::{commit_at, git_in};
use std::path::Path;

fn repo_with_commit() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    commit_at(dir.path(), "one", 1_700_000_000, "a@example.com");
    git_in(dir.path(), &["branch", "x"]);
    dir
}

/// Runs real `git` with the same pinned locale `git_cli` uses and returns the
/// failure text (stderr) of a command that is expected to fail.
fn real_git_failure(repo: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(repo)
        .env("LC_ALL", "C")
        .output_locked()
        .unwrap();
    assert!(!out.status.success(), "{args:?} was expected to fail");
    String::from_utf8_lossy(&out.stderr).trim().to_string()
}

/// Holds `lock` for `hold_ms` on a background thread, then removes it.
fn hold_lock(lock: std::path::PathBuf, hold_ms: u64) -> std::thread::JoinHandle<()> {
    std::fs::write(&lock, b"").unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(hold_ms));
        let _ = std::fs::remove_file(lock);
    })
}

/// Real git's own index.lock failure, from every git installed here (Apple git
/// 2.54 on PATH, Homebrew 2.56), is exactly the lock line plus git's advice:
/// the table in `LOCK_ADVICE_LINES` matches it for `add` and `commit` alike.
#[test]
fn every_installed_gits_index_lock_failure_matches_the_advice_table() {
    let dir = repo_with_commit();
    std::fs::write(dir.path().join(".git/index.lock"), b"").unwrap();
    std::fs::write(dir.path().join("b"), "b").unwrap();
    let mut checked = 0;
    for git in ["git", "/opt/homebrew/bin/git"] {
        if git.starts_with('/') && !Path::new(git).exists() {
            continue;
        }
        for args in [vec!["add", "b"], vec!["commit", "-m", "x", "--allow-empty"]] {
            let out = std::process::Command::new(git)
                .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
                .args(&args)
                .current_dir(dir.path())
                .env("LC_ALL", "C")
                .output_locked()
                .unwrap();
            let message = String::from_utf8_lossy(&out.stderr).trim().to_string();
            assert!(
                consists_only_of_gits_index_lock_failure(&message),
                "{git} {args:?}: {message:?}"
            );
            checked += 1;
        }
    }
    assert!(checked >= 2);
}

// ---- lock-retry classifier: git's real wording -------------------------

/// Every real lock failure git prints for the commands the app runs is
/// recognised from its first line, raw and wrapped the way git_writer wraps it.
#[test]
fn real_git_lock_messages_are_recognised_from_the_first_line() {
    let dir = repo_with_commit();
    let repo = dir.path();
    std::fs::write(repo.join("b"), "b\n").unwrap();

    let index_lock = repo.join(".git/index.lock");
    std::fs::write(&index_lock, b"").unwrap();
    for args in [
        vec!["add", "b"],
        vec!["reset"],
        vec!["checkout", "x"],
        vec!["stash", "-u"],
        vec!["commit", "-m", "two", "--allow-empty"],
    ] {
        let message = real_git_failure(repo, &args);
        assert!(
            is_transient_git_lock_error(&message),
            "{args:?}: not recognised: {message}"
        );
        let wrapped = format!(
            "Index update failed for the 3 selected paths: {message}. Refresh to review the current index."
        );
        assert!(is_transient_git_lock_error(&wrapped), "{args:?}: {wrapped}");
    }
    std::fs::remove_file(&index_lock).unwrap();

    let ref_lock = repo.join(".git/refs/heads/x.lock");
    std::fs::write(&ref_lock, b"").unwrap();
    for args in [
        vec!["update-ref", "refs/heads/x", "HEAD"],
        vec!["branch", "-f", "x"],
        vec!["branch", "-D", "x"],
    ] {
        let message = real_git_failure(repo, &args);
        assert!(
            is_transient_git_lock_error(&message),
            "{args:?}: not recognised: {message}"
        );
    }
    std::fs::remove_file(&ref_lock).unwrap();

    let head_lock = repo.join(".git/HEAD.lock");
    std::fs::write(&head_lock, b"").unwrap();
    let message = real_git_failure(repo, &["checkout", "x"]);
    assert!(is_transient_git_lock_error(&message), "{message}");
}

/// The real permanent ref failures are not locks.
#[test]
fn real_permanent_ref_failures_are_not_locks() {
    let dir = repo_with_commit();
    let repo = dir.path();
    git_in(repo, &["update-ref", "refs/heads/a", "HEAD"]);
    let conflict = real_git_failure(repo, &["update-ref", "refs/heads/a/b", "HEAD"]);
    assert!(!is_transient_git_lock_error(&conflict), "{conflict}");
    let stale = real_git_failure(
        repo,
        &[
            "update-ref",
            "refs/heads/a",
            "HEAD",
            "0000000000000000000000000000000000000001",
        ],
    );
    assert!(!is_transient_git_lock_error(&stale), "{stale}");
}

// ---- lock-retry: which commands are replayed ----------------------------

/// Control: commands outside the hook list are replayed and win once the lock
/// is released.
#[test]
fn non_hook_commands_ride_out_a_held_lock() {
    let repo = repo_with_commit();
    let path = repo.path();
    std::fs::write(path.join("b"), "b\n").unwrap();
    let t = hold_lock(path.join(".git/index.lock"), 120);
    git_text(path, &["add", "b"]).expect("add retried");
    t.join().unwrap();
    let t = hold_lock(path.join(".git/index.lock"), 120);
    git_text(path, &["reset"]).expect("reset retried");
    t.join().unwrap();
    let t = hold_lock(path.join(".git/refs/heads/x.lock"), 120);
    git_text(path, &["update-ref", "refs/heads/x", "HEAD"]).expect("update-ref retried");
    t.join().unwrap();
    let t = hold_lock(path.join(".git/index.lock"), 120);
    git_text(path, &["checkout", "x"]).expect("checkout retried");
    t.join().unwrap();
}

/// BREAK: `commit` is in `hooks::hooks_run_by`, so `lock_retry_allowed` refuses
/// every commit, with or without a hook installed. git takes `index.lock`
/// before it runs any hook, so a lock failure on commit has run nothing and is
/// the commonest contention there is. Before the change this retried.
#[test]
fn commit_without_hooks_rides_out_a_held_index_lock() {
    let repo = repo_with_commit();
    let path = repo.path();
    std::fs::write(path.join("b"), "b\n").unwrap();
    git_in(path, &["add", "b"]);
    let t = hold_lock(path.join(".git/index.lock"), 150);
    let started = Instant::now();
    let result = git_text(path, &["commit", "-m", "two"]);
    t.join().unwrap();
    assert!(
        result.is_ok(),
        "commit failed after {:?}: {result:?}",
        started.elapsed()
    );
}

/// BREAK: same class for the other commands in `hooks_run_by` that fail on the
/// index/ref lock before any hook runs (merge, cherry-pick, revert, rebase).
#[test]
fn merge_and_cherry_pick_without_hooks_ride_out_a_held_index_lock() {
    let repo = repo_with_commit();
    let path = repo.path();
    git_in(path, &["checkout", "-q", "x"]);
    // A real change: `commit_at` commits empty, and cherry-picking an empty
    // commit stops with "now empty" whatever the lock did.
    std::fs::write(path.join("side.txt"), "side\n").unwrap();
    git_in(path, &["add", "side.txt"]);
    commit_at(path, "side", 1_700_000_100, "a@example.com");
    git_in(path, &["checkout", "-q", "main"]);
    let t = hold_lock(path.join(".git/index.lock"), 150);
    let merged = git_text(path, &["merge", "--no-ff", "--no-edit", "x"]);
    t.join().unwrap();
    assert!(merged.is_ok(), "merge: {merged:?}");
    git_in(path, &["reset", "-q", "--hard", "HEAD~1"]);
    let t = hold_lock(path.join(".git/index.lock"), 150);
    let picked = git_text(path, &["cherry-pick", "x"]);
    t.join().unwrap();
    assert!(picked.is_ok(), "cherry-pick: {picked:?}");
}

/// A commit that fails on a lock that is never released backs off and then
/// gives up with git's own message: it is retried (the first attempt is not
/// final), and the retries are bounded. This pinned the opposite while commit
/// was wholesale excluded from the lock retry.
#[test]
fn commit_on_a_held_lock_is_retried_then_fails_with_gits_message() {
    let repo = repo_with_commit();
    let path = repo.path();
    std::fs::write(path.join("b"), "b\n").unwrap();
    git_in(path, &["add", "b"]);
    let lock = path.join(".git/index.lock");
    std::fs::write(&lock, b"").unwrap();
    let started = Instant::now();
    let error = git_text(path, &["commit", "-m", "two"]).expect_err("lock held");
    assert!(is_transient_git_lock_error(&error), "{error}");
    assert!(
        started.elapsed() >= Duration::from_millis(450),
        "no backoff happened: {:?}",
        started.elapsed()
    );
    std::fs::remove_file(lock).unwrap();
}

/// A hook-running command is replayed only for a lock in THIS repository's git
/// directory; a lock line naming another path (a nested git in a hook, another
/// repository) is not.
#[test]
fn hooked_commands_retry_only_for_a_lock_in_their_own_git_dir() {
    let repo = repo_with_commit();
    let other = repo_with_commit();
    let path = repo.path();
    let advice = "\n\nAnother git process seems to be running in this repository, or the lock file may be stale";
    let own = format!(
        "fatal: Unable to create '{}/.git/index.lock': File exists.{advice}",
        path.display()
    );
    let foreign = format!(
        "fatal: Unable to create '{}/.git/index.lock': File exists.{advice}",
        other.path().display()
    );
    // The same lock line without git's advice block is not git's own failure.
    let bare = format!(
        "fatal: Unable to create '{}/.git/index.lock': File exists.",
        path.display()
    );
    assert!(!lock_retry_wanted(Some(path), "commit", &bare));
    assert!(lock_retry_wanted(Some(path), "add", &bare));
    for sub in ["commit", "merge", "cherry-pick", "revert", "rebase", "am"] {
        assert!(lock_retry_wanted(Some(path), sub, &own), "{sub}");
        assert!(!lock_retry_wanted(Some(path), sub, &foreign), "{sub}");
        assert!(!lock_retry_wanted(None, sub, &own), "{sub}: no repo");
    }
    for sub in ["push", "fetch", "pull", "clone", "ls-remote"] {
        assert!(!lock_retry_wanted(Some(path), sub, &own), "{sub}");
    }
    assert!(lock_retry_wanted(Some(path), "add", &foreign));
}

/// A lock failure that happens before any hook ran says nothing was run. The
/// hook never executes, so replaying cannot run it twice. (Documents why the
/// wholesale exclusion of `commit` is unnecessary.)
#[cfg(unix)]
#[test]
fn a_lock_failure_on_commit_happens_before_the_pre_commit_hook() {
    use std::os::unix::fs::PermissionsExt;
    let repo = repo_with_commit();
    let path = repo.path();
    std::fs::write(path.join("b"), "b\n").unwrap();
    git_in(path, &["add", "b"]);
    let marker = path.join("hook-ran");
    let hook = path.join(".git/hooks/pre-commit");
    std::fs::create_dir_all(hook.parent().unwrap()).unwrap();
    std::fs::write(
        &hook,
        format!("#!/bin/sh\necho ran >> '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::fs::write(path.join(".git/index.lock"), b"").unwrap();
    let message = real_git_failure(path, &["commit", "-m", "two"]);
    assert!(is_transient_git_lock_error(&message), "{message}");
    assert!(!marker.exists(), "the hook ran before the lock failed");
}

/// `checkout` runs post-checkout, which `hooks_run_by` does not list; it is
/// replayed. Safe because the lock failure precedes the hook.
#[test]
fn checkout_is_replayed_on_a_lock() {
    assert!(lock_retry_allowed("checkout"));
    assert!(lock_retry_allowed("stash"));
    assert!(lock_retry_allowed("switch"));
}

/// Network commands that fail on a LOCAL ref lock (fetch updating
/// refs/remotes/*) are no longer replayed either. Deliberate in the diff;
/// pinned here so a reader sees it.
#[test]
fn fetch_and_pull_are_never_replayed_even_for_a_local_ref_lock() {
    for sub in ["fetch", "pull", "push", "clone", "ls-remote"] {
        assert!(!lock_retry_allowed(sub), "{sub}");
    }
}

// ---- lock-retry fixture ---------------------------------------------------

/// The fixture wording must be what git prints for a real ref lock, and the
/// classifier must accept it.
#[test]
fn the_audit_fixture_wording_is_real_git_wording() {
    let repo = repo_with_commit();
    let lock = repo.path().join(".git/refs/heads/x.lock");
    std::fs::write(&lock, b"").unwrap();
    let message = real_git_failure(repo.path(), &["update-ref", "refs/heads/x", "HEAD"]);
    assert!(
        message.contains("cannot lock ref 'refs/heads/x': Unable to create")
            && message.contains(".lock': File exists."),
        "{message}"
    );
    assert!(is_transient_git_lock_error(&message));
    // The fixture the audit test now uses.
    assert!(is_transient_git_lock_error(
        "fatal: Unable to create 'x.lock': File exists."
    ));
}

/// The old fixture, a bare "cannot lock ref", is not a real LOCK failure: git
/// prints it for the permanent forms. It must not be retried.
#[test]
fn bare_cannot_lock_ref_is_not_a_transient_lock() {
    assert!(!is_transient_git_lock_error("cannot lock ref"));
    assert!(!is_transient_git_lock_error(
        "error: cannot lock ref 'refs/heads/x': reference already exists"
    ));
    assert!(!is_transient_git_lock_error(
        "error: cannot lock ref 'HEAD': unable to resolve reference 'HEAD': reference broken"
    ));
}

/// BREAK (narrow): git's other real transient shape for a ref lock, a racing
/// directory create, says `unable to create directory for '...lock'` with no
/// `File exists`. The old classifier retried it (unable to create + .lock);
/// the new one never does.
#[test]
fn racing_ref_directory_create_is_still_transient() {
    assert!(is_transient_git_lock_error(
        "error: cannot lock ref 'refs/heads/a/b': unable to create directory for '/r/.git/refs/heads/a/b.lock'"
    ));
}

/// Path text that merely mentions a rejected phrase on git's first line.
/// BREAK: the guard words are matched anywhere on the line, so a repository
/// whose path contains "remote:" or "but expected" is never retried.
#[test]
fn a_path_containing_a_guard_word_is_still_a_lock() {
    assert!(is_transient_git_lock_error(
        "fatal: Unable to create '/Users/me/remote: backups/r/.git/index.lock': File exists."
    ));
    assert!(is_transient_git_lock_error(
        "fatal: Unable to create '/Users/me/but expected/r/.git/index.lock': File exists."
    ));
}

/// A lock failure whose first line is a hint or warning git printed first
/// (git prints these for some configurations) is not retried: fails closed.
#[test]
fn a_leading_warning_line_hides_the_lock() {
    assert!(!is_transient_git_lock_error(
        "warning: something\nfatal: Unable to create '/r/.git/index.lock': File exists."
    ));
}

// ---- RunError ---------------------------------------------------------------

#[test]
fn run_error_message_shapes() {
    let slot = format!("devmap{TIMEOUT_MARKER}0.5{SLOT_WAIT_SUFFIX}");
    let ran = format!("devmap{TIMEOUT_MARKER}30s");
    let deferred = refusal_message(
        "devmap build",
        Refusal::Refused {
            waited: Duration::from_millis(500),
        },
    );
    // raw, coded, doubly coded
    assert_eq!(RunError::classify(&ran), RunError::Timeout);
    assert_eq!(RunError::classify(&format!("[a] {ran}")), RunError::Timeout);
    assert_eq!(
        RunError::classify(&format!("[a] [b] {ran}")),
        RunError::Other,
        "one code prefix is stripped, for every anchored check alike"
    );
    assert_eq!(
        RunError::classify(&format!("[a] [b] {deferred}")),
        RunError::Other,
        "nested code prefix is not stripped by is_deferred_under_load"
    );
    // a label that spells "deferred"
    assert_eq!(
        RunError::classify(&format!("devmap deferred{TIMEOUT_MARKER}1s")),
        RunError::Timeout
    );
    // a path containing the timeout phrase is only a timeout if the message is
    // the runner's: an exit-status failure that names such a path is not.
    assert_eq!(
        RunError::classify("devmap exited with status 2: /tmp/job timed out after 3 retries/x"),
        RunError::Other,
        "exit-failure text that merely contains the marker is not the runner's deadline"
    );
    assert_eq!(RunError::classify(&slot), RunError::SlotWaitTimeout);
    assert_eq!(RunError::classify(&deferred), RunError::Deferred);
}

/// BREAK: a caller-prefixed spawn failure whose program path spells the timeout
/// phrase still reads as a timeout: the `Failed to spawn` check is anchored at
/// the start of the message, so any prefix (a `[code]` or a caller wrapper)
/// defeats the precedence the doc comment promises.
#[test]
fn prefixed_spawn_failure_with_hostile_path_is_not_a_timeout() {
    let hostile =
        format!("Failed to spawn /Users/x/devmap{TIMEOUT_MARKER}1s/devmap: permission denied");
    assert!(!is_run_timeout(&format!("[spawn_failed] {hostile}")));
    assert!(!is_run_timeout(&format!("Install failed: {hostile}")));
}

/// BREAK: a child's own stderr relayed through a failure message that happens
/// to carry " timed out after " (curl and cargo print "Operation timed out
/// after 30001 milliseconds") reads as the runner's deadline.
#[test]
fn a_relayed_child_timeout_is_not_a_runner_timeout() {
    assert!(!is_run_timeout(
        "git fetch failed: fatal: unable to access 'https://h/r.git/': Operation timed out after 30001 milliseconds with 0 bytes received"
    ));
}

/// A real runner timeout, from a real slow child and a tiny deadline, through
/// the exact runners devmap::cli (`run_observed`) and tool_install
/// (`run_bounded`) use, is still `Timeout`.
#[cfg(unix)]
#[test]
fn a_real_slow_child_is_classified_as_a_timeout_by_both_runners() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::TempDir::new().unwrap();
    let bin = dir.path().join("devmap");
    std::fs::write(&bin, "#!/bin/sh\nsleep 5\n").unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();

    let err = run_bounded(
        Command::new(&bin),
        "devmap",
        Duration::from_millis(200),
        None,
    )
    .expect_err("slow child must time out");
    assert!(is_run_timeout(&err), "{err}");
    assert_eq!(RunError::classify(&err), RunError::Timeout, "{err}");

    let mut cmd = Command::new(&bin);
    let err = run_observed(
        &mut cmd,
        "devmap",
        Duration::from_millis(200),
        None,
        MAX_OUTPUT_BYTES,
        &mut (),
    )
    .expect_err("slow child must time out");
    assert!(is_run_timeout(&err), "{err}");

    let missing = run_bounded(
        Command::new(dir.path().join("nope")),
        "devmap",
        Duration::from_secs(5),
        None,
    )
    .expect_err("missing binary");
    assert_eq!(
        RunError::classify(&missing),
        RunError::SpawnNotFound,
        "{missing}"
    );
}

// ---- unborn HEAD -------------------------------------------------------------

fn head_error(repo: &Path) -> String {
    git_text(repo, &["log", "-n1", "--format=%H"]).expect_err("HEAD does not resolve")
}

#[test]
fn fresh_init_and_orphan_branch_are_unborn() {
    let fresh = tempfile::TempDir::new().unwrap();
    git_in(fresh.path(), &["init", "-q", "-b", "main"]);
    assert!(head_is_unborn(fresh.path(), &head_error(fresh.path())));
    let p = fresh.path().to_str().unwrap();
    assert!(GitReader::pulse_report(p, None).unwrap().commits.is_empty());
    assert!(GitReader::dora_report(p, Some(90)).is_ok());

    let repo = repo_with_commit();
    git_in(repo.path(), &["checkout", "-q", "--orphan", "o"]);
    assert!(head_is_unborn(repo.path(), &head_error(repo.path())));
    // `main` still has history, so the named walk is an answer, not an error.
    assert!(GitReader::pulse_report(repo.path().to_str().unwrap(), None).is_ok());
}

#[test]
fn a_linked_worktree_with_an_unborn_branch_is_unborn() {
    let repo = repo_with_commit();
    let wt_parent = tempfile::TempDir::new().unwrap();
    let wt = wt_parent.path().join("wt");
    git_in(
        repo.path(),
        &[
            "worktree",
            "add",
            "-q",
            "--orphan",
            "-b",
            "wtb",
            wt.to_str().unwrap(),
        ],
    );
    assert!(head_is_unborn(&wt, &head_error(&wt)), "{}", head_error(&wt));
}

#[test]
fn a_damaged_loose_ref_is_not_unborn() {
    for garbage in ["garbage\n", ""] {
        let repo = repo_with_commit();
        git_in(repo.path(), &["branch", "-D", "x"]);
        std::fs::write(repo.path().join(".git/refs/heads/main"), garbage).unwrap();
        let err = head_error(repo.path());
        assert!(!head_is_unborn(repo.path(), &err), "{garbage:?}: {err}");
        let p = repo.path().to_str().unwrap();
        assert!(GitReader::pulse_report(p, None).is_err(), "{garbage:?}");
        assert!(GitReader::dora_report(p, Some(90)).is_err(), "{garbage:?}");
    }
}

#[test]
fn a_packed_only_ref_resolves_and_is_not_unborn() {
    let repo = repo_with_commit();
    git_in(repo.path(), &["pack-refs", "--all"]);
    assert!(!repo.path().join(".git/refs/heads/main").exists());
    let p = repo.path().to_str().unwrap();
    assert_eq!(GitReader::pulse_report(p, None).unwrap().commits.len(), 1);
}

/// BREAK: git calls a branch whose ref path is an (empty) DIRECTORY unborn
/// (`your current branch 'main' does not have any commits yet`), but
/// `head_ref_is_absent` tests `Path::exists`, which is true for a directory. A
/// leftover empty `refs/heads/feature/` directory after `feature/x` was
/// deleted, with HEAD on a new unborn `feature`, is a real unborn repository
/// that is reported as an error.
#[test]
fn an_unborn_branch_whose_ref_path_is_an_empty_directory_is_unborn() {
    let dir = tempfile::TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    std::fs::create_dir_all(dir.path().join(".git/refs/heads/main")).unwrap();
    let err = head_error(dir.path());
    assert!(err.contains("does not have any commits"), "{err}");
    assert!(head_is_unborn(dir.path(), &err), "{err}");
    let p = dir.path().to_str().unwrap();
    assert!(GitReader::pulse_report(p, None).unwrap().commits.is_empty());
    assert!(GitReader::dora_report(p, Some(90)).is_ok());
}

/// Reftable: refs are not files, so `rev-parse --git-path refs/heads/main`
/// names a path that never exists. An unborn branch is correctly unborn; the
/// absent-file test cannot tell a present-but-broken reftable ref from an
/// absent one (see the report: unverified, no way found to damage one).
#[test]
fn a_fresh_reftable_repo_is_unborn() {
    let dir = tempfile::TempDir::new().unwrap();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "--ref-format=reftable"])
        .arg(dir.path())
        .output_locked()
        .unwrap();
    if !init.status.success() {
        return;
    }
    // GitPulse's repository trust gate refuses a reftable repository before
    // any read, so the classifier is never asked: the git probe itself says so.
    let error = git_text(dir.path(), &["log", "-n1", "--format=%H"]).expect_err("no commits");
    assert!(
        error.contains("REPOSITORY_TRUST_REQUIRED") || head_is_unborn(dir.path(), &error),
        "{error}"
    );
}

/// A deferred or timed-out probe never reads as unborn.
#[test]
fn a_probe_that_did_not_run_is_not_unborn() {
    let dir = tempfile::TempDir::new().unwrap();
    git_in(dir.path(), &["init", "-q", "-b", "main"]);
    for err in [
        "git log timed out after 90s",
        "git log deferred under load after 0.500s: the git spawn rate limit admitted nothing sooner",
        "fatal: not a git repository",
    ] {
        assert!(!head_is_unborn(dir.path(), err), "{err}");
    }
}
