//! End-to-end coverage for parked-operation detection and recovery against
//! real repositories driven by the real `git` binary.
//!
//! Every case here parks a repository the way a user's mistake would, asserts
//! GitPulse can see and name the state, drives the recovery verb, and then
//! asserts the repository is genuinely back to a clean, idle state — not just
//! that the command exited zero.

use gitpulse_lib::engine::repo_op::{self, OperationAction, OperationKind};

/// Drives a recovery verb with a judge that records the argv and approves it.
///
/// `run_action_with` takes the gate as a parameter precisely so no ungated
/// entry point exists; spelling the no-op judge out here keeps the absence of
/// a policy gate visible at every call site rather than hidden in a helper
/// that reads like production code.
fn act(repo_path: &str, action: OperationAction) -> Result<String, String> {
    repo_op::run_action_with(repo_path, action, |argv| {
        assert_eq!(
            argv.first().copied(),
            Some("git"),
            "judged argv must be a git line"
        );
        Ok(())
    })
    .map(|((), output)| output)
}
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

struct TestRepo {
    dir: TempDir,
}

impl TestRepo {
    fn init() -> Self {
        let dir = TempDir::new().expect("tempdir");
        run_git(dir.path(), &["init", "-b", "main"]);
        run_git(dir.path(), &["config", "user.email", "test@example.com"]);
        run_git(dir.path(), &["config", "user.name", "Test User"]);
        run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
        // Windows installs git with core.autocrlf=true in its system config, so a
        // fixture repo inherits it and git rewrites LF to CRLF on checkout --
        // silently breaking every assertion below that compares exact bytes.
        // These tests own their line endings; pin the policy rather than
        // inherit the host's.
        run_git(dir.path(), &["config", "core.autocrlf", "false"]);
        Self { dir }
    }

    fn path(&self) -> &Path {
        self.dir.path()
    }

    fn path_str(&self) -> String {
        self.dir.path().to_string_lossy().into_owned()
    }

    fn write(&self, rel: &str, content: &str) {
        let dest = self.dir.path().join(rel);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(dest, content).unwrap();
    }

    fn commit_all(&self, message: &str) {
        run_git(self.dir.path(), &["add", "-A"]);
        run_git(self.dir.path(), &["commit", "-m", message]);
    }

    /// Builds `main` and `side` that both edit `f.txt`, so any replay of one
    /// onto the other conflicts.
    fn with_diverged_branches() -> Self {
        let repo = Self::init();
        repo.write("f.txt", "base\n");
        repo.commit_all("base");
        run_git(repo.path(), &["checkout", "-b", "side"]);
        repo.write("f.txt", "side\n");
        repo.commit_all("side change");
        run_git(repo.path(), &["checkout", "main"]);
        repo.write("f.txt", "main\n");
        repo.commit_all("main change");
        repo
    }

    fn head_oid(&self) -> String {
        git_out(self.path(), &["rev-parse", "HEAD"])
    }

    fn current_branch(&self) -> String {
        git_out(self.path(), &["rev-parse", "--abbrev-ref", "HEAD"])
    }

    fn is_clean(&self) -> bool {
        git_out(self.path(), &["status", "--porcelain"]).is_empty()
    }
}

fn run_git(cwd: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .status()
        .expect("git");
    assert!(status.success(), "git {args:?} failed");
}

/// Runs git ignoring the exit status — for the commands that are *supposed* to
/// fail because they hit a conflict.
fn run_git_allow_failure(cwd: &Path, args: &[&str]) {
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

// --- detection ----------------------------------------------------------

#[test]
fn idle_repository_reports_no_operation() {
    let repo = TestRepo::init();
    repo.write("a.txt", "a\n");
    repo.commit_all("one");
    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
}

#[test]
fn conflicted_merge_is_detected_and_described() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("merge must be detected");
    assert_eq!(op.kind, OperationKind::Merge);
    assert_eq!(op.head_ref.as_deref(), Some("main"));
    assert_eq!(op.conflicted_total, 1);
    assert_eq!(op.conflicted_paths, vec!["f.txt".to_string()]);
    // A merge is a single step and must never advertise one.
    assert_eq!(op.current_step, None);
    assert_eq!(op.total_steps, None);
    // Continue is withheld while the index is unmerged; abort always stands.
    assert!(op.allows(OperationAction::Abort));
    assert!(!op.allows(OperationAction::Continue));
    assert!(!op.allows(OperationAction::Skip));
    assert!(
        op.warnings.is_empty(),
        "unexpected warnings: {:?}",
        op.warnings
    );
}

/// The regression that motivated the module: `git cherry-pick` writes
/// `CHERRY_PICK_HEAD` and touches neither `MERGE_HEAD` nor `rebase-*`, so the
/// pre-fix predicate reported a parked cherry-pick as an idle repository.
#[test]
fn conflicted_cherry_pick_is_detected() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["cherry-pick", "side"]);

    assert!(
        repo.path().join(".git/CHERRY_PICK_HEAD").exists(),
        "precondition: git parked a cherry-pick"
    );
    assert!(
        !repo.path().join(".git/MERGE_HEAD").exists()
            && !repo.path().join(".git/rebase-merge").exists()
            && !repo.path().join(".git/rebase-apply").exists(),
        "precondition: none of the pre-fix probes fire here"
    );

    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("cherry-pick must be detected");
    assert_eq!(op.kind, OperationKind::CherryPick);
    assert_eq!(op.conflicted_total, 1);
    assert!(op.allows(OperationAction::Abort));
    assert!(op.allows(OperationAction::Skip));
}

/// Same class as the cherry-pick case: `REVERT_HEAD` only.
#[test]
fn conflicted_revert_is_detected() {
    let repo = TestRepo::init();
    repo.write("f.txt", "v1\n");
    repo.commit_all("c1");
    repo.write("f.txt", "v2\n");
    repo.commit_all("c2");
    repo.write("f.txt", "v3\n");
    repo.commit_all("c3");
    run_git_allow_failure(repo.path(), &["revert", "--no-edit", "HEAD~1"]);

    assert!(repo.path().join(".git/REVERT_HEAD").exists());
    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("revert must be detected");
    assert_eq!(op.kind, OperationKind::Revert);
    assert_eq!(op.kind.label(), "revert");
}

#[test]
fn conflicted_rebase_reports_step_progress_and_the_rebased_branch() {
    let repo = TestRepo::init();
    repo.write("f.txt", "a\n");
    repo.commit_all("c1");
    repo.write("f.txt", "b\n");
    repo.commit_all("c2");
    run_git(repo.path(), &["checkout", "-b", "side", "HEAD~1"]);
    repo.write("f.txt", "c\n");
    repo.commit_all("c3");
    repo.write("f.txt", "d\n");
    repo.commit_all("c4");
    run_git_allow_failure(repo.path(), &["rebase", "main"]);

    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("rebase must be detected");
    assert!(matches!(
        op.kind,
        OperationKind::Rebase | OperationKind::RebaseApply
    ));
    // HEAD is detached during a rebase; the branch name must still come back.
    assert_eq!(op.head_ref.as_deref(), Some("side"));
    assert_eq!(op.current_step, Some(1));
    assert_eq!(op.total_steps, Some(2));
    assert!(op.allows(OperationAction::Skip));
}

#[test]
fn multi_commit_cherry_pick_reports_sequencer_progress() {
    let repo = TestRepo::init();
    repo.write("f.txt", "base\n");
    repo.commit_all("base");
    run_git(repo.path(), &["checkout", "-b", "side"]);
    repo.write("f.txt", "one\n");
    repo.commit_all("one");
    repo.write("g.txt", "two\n");
    repo.commit_all("two");
    run_git(repo.path(), &["checkout", "main"]);
    repo.write("f.txt", "diverged\n");
    repo.commit_all("diverged");

    run_git_allow_failure(repo.path(), &["cherry-pick", "side~1", "side"]);
    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("cherry-pick must be detected");
    assert_eq!(op.kind, OperationKind::CherryPick);
    // Parked on the first of two picks.
    assert_eq!(op.current_step, Some(1));
    assert_eq!(op.total_steps, Some(2));
}

/// A rebase is detected inside the linked worktree that owns it, and the
/// *other* worktrees of the same repository stay idle. Resolving through the
/// shared common dir instead of the per-worktree git dir would report the
/// rebase in all of them and offer an abort that unwinds someone else's work.
#[test]
fn operations_are_scoped_to_the_worktree_that_owns_them() {
    let repo = TestRepo::init();
    repo.write("f.txt", "a\n");
    repo.commit_all("c1");
    repo.write("f.txt", "b\n");
    repo.commit_all("c2");
    run_git(repo.path(), &["branch", "side", "HEAD~1"]);

    // The linked worktree lives in its own TempDir rather than beside the
    // repository: a fixed name under the shared temp root collides with any
    // concurrent or previous run, and `git worktree add` then fails on a path
    // that already exists.
    let host = TempDir::new().expect("tempdir");
    let linked = host.path().join("linked-wt");
    let linked_arg = linked.to_string_lossy().into_owned();
    run_git(repo.path(), &["worktree", "add", &linked_arg, "side"]);
    let linked = linked.canonicalize().unwrap();

    // Make `side` conflict with main inside the linked worktree only.
    fs::write(linked.join("f.txt"), "c\n").unwrap();
    run_git(&linked, &["add", "-A"]);
    run_git(&linked, &["commit", "-m", "side change"]);
    run_git_allow_failure(&linked, &["rebase", "main"]);

    let in_linked = repo_op::detect(&linked).unwrap();
    assert!(in_linked.is_some(), "the linked worktree owns the rebase");

    let in_main = repo_op::detect(repo.path()).unwrap();
    assert_eq!(
        in_main, None,
        "the main worktree must stay idle; got {in_main:?}"
    );
}

// --- recovery -----------------------------------------------------------

#[test]
fn aborting_a_merge_restores_the_pre_merge_head_and_a_clean_tree() {
    let repo = TestRepo::with_diverged_branches();
    let before = repo.head_oid();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);
    assert!(repo_op::detect(repo.path()).unwrap().is_some());

    act(&repo.path_str(), OperationAction::Abort).unwrap();

    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert_eq!(repo.head_oid(), before);
    assert!(repo.is_clean(), "abort must leave a clean worktree");
    assert_eq!(
        fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "main\n"
    );
}

#[test]
fn aborting_a_cherry_pick_restores_the_pre_pick_state() {
    let repo = TestRepo::with_diverged_branches();
    let before = repo.head_oid();
    run_git_allow_failure(repo.path(), &["cherry-pick", "side"]);

    act(&repo.path_str(), OperationAction::Abort).unwrap();

    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert_eq!(repo.head_oid(), before);
    assert!(repo.is_clean());
}

#[test]
fn aborting_a_rebase_returns_to_the_original_branch() {
    let repo = TestRepo::init();
    repo.write("f.txt", "a\n");
    repo.commit_all("c1");
    repo.write("f.txt", "b\n");
    repo.commit_all("c2");
    run_git(repo.path(), &["checkout", "-b", "side", "HEAD~1"]);
    repo.write("f.txt", "c\n");
    repo.commit_all("c3");
    let before = repo.head_oid();
    run_git_allow_failure(repo.path(), &["rebase", "main"]);

    act(&repo.path_str(), OperationAction::Abort).unwrap();

    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert_eq!(repo.current_branch(), "side");
    assert_eq!(repo.head_oid(), before);
}

/// The full user journey: conflict, edit the file, stage it, continue — and
/// the merge is concluded as a real merge commit with two parents.
#[test]
fn resolving_then_continuing_concludes_the_merge() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    // Continue is refused while the conflict stands — and says why.
    let refused = act(&repo.path_str(), OperationAction::Continue).unwrap_err();
    assert!(
        refused.contains("conflict"),
        "refusal must name the cause, got: {refused}"
    );

    repo.write("f.txt", "resolved\n");
    run_git(repo.path(), &["add", "f.txt"]);

    let op = repo_op::detect(repo.path()).unwrap().unwrap();
    assert_eq!(op.conflicted_total, 0);
    assert!(op.allows(OperationAction::Continue));

    act(&repo.path_str(), OperationAction::Continue).unwrap();

    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert!(repo.is_clean());
    // Two parents is what makes this a merge rather than a squash.
    let parents = git_out(repo.path(), &["rev-list", "--parents", "-n", "1", "HEAD"]);
    assert_eq!(
        parents.split_whitespace().count(),
        3,
        "expected commit + 2 parents, got: {parents}"
    );
}

#[test]
fn skipping_a_cherry_pick_step_drops_only_that_commit() {
    let repo = TestRepo::init();
    repo.write("f.txt", "base\n");
    repo.commit_all("base");
    run_git(repo.path(), &["checkout", "-b", "side"]);
    repo.write("f.txt", "one\n");
    repo.commit_all("one");
    repo.write("g.txt", "two\n");
    repo.commit_all("two");
    run_git(repo.path(), &["checkout", "main"]);
    repo.write("f.txt", "diverged\n");
    repo.commit_all("diverged");

    run_git_allow_failure(repo.path(), &["cherry-pick", "side~1", "side"]);
    // Skip the conflicting first pick; the second one applies cleanly and the
    // sequencer runs to completion.
    act(&repo.path_str(), OperationAction::Skip).unwrap();

    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    // The dropped commit's change is absent, the kept one's is present.
    assert_eq!(
        fs::read_to_string(repo.path().join("f.txt")).unwrap(),
        "diverged\n"
    );
    assert!(
        repo.path().join("g.txt").exists(),
        "the second pick applied"
    );
}

#[test]
fn a_merge_cannot_be_skipped() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);
    let err = act(&repo.path_str(), OperationAction::Skip).unwrap_err();
    assert!(err.contains("skip"), "got: {err}");
    // Refusal must leave the operation exactly as it was, not half-unwound.
    assert_eq!(
        repo_op::detect(repo.path()).unwrap().unwrap().kind,
        OperationKind::Merge
    );
}

#[test]
fn acting_on_an_idle_repository_is_refused_rather_than_run() {
    let repo = TestRepo::init();
    repo.write("a.txt", "a\n");
    repo.commit_all("one");
    let head = repo.head_oid();

    for action in [
        OperationAction::Abort,
        OperationAction::Continue,
        OperationAction::Skip,
    ] {
        let err = act(&repo.path_str(), action).unwrap_err();
        assert!(
            err.contains("No merge"),
            "{action:?} must be refused with a usable message, got: {err}"
        );
    }
    assert_eq!(repo.head_oid(), head, "a refusal must change nothing");
    assert!(repo.is_clean());
}

// --- interaction with the rest of the writer ---------------------------

/// The restack preflight consults the same detector, so a parked cherry-pick
/// now blocks a restack and the refusal names the real operation. Pre-fix it
/// said "rebase or merge" for every state — and did not see this one at all.
#[test]
fn restack_refuses_on_a_parked_cherry_pick_and_names_it() {
    use gitpulse_lib::engine::GitWriter;

    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["cherry-pick", "side"]);

    let canon = repo.path().canonicalize().unwrap();
    let err = GitWriter::execute_restack(&canon, "side", "main", "main").unwrap_err();
    assert!(
        err.contains("cherry-pick"),
        "refusal must name the parked operation, got: {err}"
    );
    // And the cherry-pick is still there, untouched.
    assert_eq!(
        repo_op::detect(repo.path()).unwrap().unwrap().kind,
        OperationKind::CherryPick
    );
}

// --- adversarial --------------------------------------------------------

/// Corrupt counters degrade the *progress* field only. The operation itself
/// must still be detected and must still be abortable — a repository the user
/// cannot escape is a worse outcome than a missing "step 2 of 5".
#[test]
fn corrupt_step_counters_degrade_progress_without_hiding_the_operation() {
    let repo = TestRepo::init();
    repo.write("f.txt", "a\n");
    repo.commit_all("c1");
    repo.write("f.txt", "b\n");
    repo.commit_all("c2");
    run_git(repo.path(), &["checkout", "-b", "side", "HEAD~1"]);
    repo.write("f.txt", "c\n");
    repo.commit_all("c3");
    run_git_allow_failure(repo.path(), &["rebase", "main"]);

    let msgnum = repo.path().join(".git/rebase-merge/msgnum");
    if msgnum.exists() {
        fs::write(&msgnum, "garbage\n").unwrap();
        let op = repo_op::detect(repo.path()).unwrap().unwrap();
        assert_eq!(
            op.current_step, None,
            "a corrupt counter must not be guessed"
        );
        assert!(
            !op.warnings.is_empty(),
            "a degraded read must be reported, not silently dropped"
        );
        assert!(op.allows(OperationAction::Abort), "escape must survive");
    }
}

/// A `.git` control file large enough to be a memory hazard is skipped with a
/// warning rather than read into the UI process.
#[test]
fn oversized_control_files_are_refused_not_read() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    let merge_msg = repo.path().join(".git/MERGE_MSG");
    fs::write(&merge_msg, vec![b'x'; 5 * 1024 * 1024]).unwrap();

    let op = repo_op::detect(repo.path()).unwrap().unwrap();
    assert_eq!(op.kind, OperationKind::Merge);
    assert!(
        op.warnings.iter().any(|w| w.contains("cap")),
        "the skip must be reported: {:?}",
        op.warnings
    );
    assert!(op.allows(OperationAction::Abort));
}

/// Detection must never turn a control file's contents into git arguments.
#[test]
fn control_file_contents_never_become_git_arguments() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["cherry-pick", "side"]);

    // Overwrite the OID record with something that would be catastrophic if it
    // reached argv unchecked.
    let marker = repo.path().join(".git/CHERRY_PICK_HEAD");
    fs::write(&marker, "--upload-pack=touch /tmp/gitpulse-pwned\n").unwrap();

    let op = repo_op::detect(repo.path()).unwrap().unwrap();
    assert_eq!(op.kind, OperationKind::CherryPick);
    // The shape check rejected it, so no description was produced and no git
    // invocation carried the string.
    assert_eq!(op.incoming_ref, None);
    assert!(!Path::new("/tmp/gitpulse-pwned").exists());
}

/// Repeated detection is a pure read: it must not perturb the repository, and
/// it must return the same answer every time. Detection runs on every status
/// refresh, so a detector with side effects would corrupt state under polling.
#[test]
fn detection_is_idempotent_and_side_effect_free() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);
    let head = repo.head_oid();
    let status = git_out(repo.path(), &["status", "--porcelain"]);

    let first = repo_op::detect(repo.path()).unwrap();
    for _ in 0..25 {
        assert_eq!(repo_op::detect(repo.path()).unwrap(), first);
    }
    assert_eq!(repo.head_oid(), head);
    assert_eq!(git_out(repo.path(), &["status", "--porcelain"]), status);
}

/// Concurrent recovery attempts must not both act. `run_action` re-detects
/// under the repository mutation lock, so the loser sees an idle repository
/// and refuses instead of running `--abort` against nothing.
#[test]
fn concurrent_aborts_leave_exactly_one_winner() {
    let repo = TestRepo::with_diverged_branches();
    let before = repo.head_oid();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    let path = repo.path_str();
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let path = path.clone();
            std::thread::spawn(move || act(&path, OperationAction::Abort))
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let winners = results.iter().filter(|r| r.is_ok()).count();
    assert_eq!(winners, 1, "exactly one abort may run: {results:?}");
    for failure in results.iter().filter_map(|r| r.as_ref().err()) {
        assert!(
            failure.contains("No merge"),
            "losers must report an idle repo, got: {failure}"
        );
    }
    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert_eq!(repo.head_oid(), before);
    assert!(repo.is_clean());
}

/// Paths that are awkward for shells and for `-z` parsing must survive into
/// the conflicted-path list intact.
#[test]
fn conflicted_paths_with_spaces_and_unicode_survive() {
    let repo = TestRepo::init();
    let awkward = "dir with spaces/ünïcode — file.txt";
    repo.write(awkward, "base\n");
    repo.commit_all("base");
    run_git(repo.path(), &["checkout", "-b", "side"]);
    repo.write(awkward, "side\n");
    repo.commit_all("side");
    run_git(repo.path(), &["checkout", "main"]);
    repo.write(awkward, "main\n");
    repo.commit_all("main");
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    let op = repo_op::detect(repo.path()).unwrap().unwrap();
    assert_eq!(op.conflicted_total, 1);
    assert_eq!(op.conflicted_paths, vec![awkward.to_string()]);
}

/// A bisect is an operation the user must be able to leave, and its only
/// escape is `git bisect reset` — not `--abort`.
#[test]
fn a_running_bisect_is_detected_and_reset_clears_it() {
    let repo = TestRepo::init();
    for i in 1..=5 {
        repo.write("f.txt", &format!("v{i}\n"));
        repo.commit_all(&format!("c{i}"));
    }
    let head = repo.head_oid();
    run_git(repo.path(), &["bisect", "start"]);
    run_git(repo.path(), &["bisect", "bad", "HEAD"]);
    run_git_allow_failure(repo.path(), &["bisect", "good", "HEAD~4"]);

    let op = repo_op::detect(repo.path())
        .unwrap()
        .expect("bisect must be detected");
    assert_eq!(op.kind, OperationKind::Bisect);
    assert_eq!(op.available, vec![OperationAction::Abort]);

    act(&repo.path_str(), OperationAction::Abort).unwrap();
    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
    assert_eq!(
        repo.head_oid(),
        head,
        "reset must restore the original HEAD"
    );
}

// --- the write gate ----------------------------------------------------

/// A refusing gate must stop the verb before git runs, not after.
#[test]
fn a_refused_action_does_not_touch_the_repository() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);
    let head = repo.head_oid();
    let status = git_out(repo.path(), &["status", "--porcelain"]);

    let err = repo_op::run_action_with(&repo.path_str(), OperationAction::Abort, |_argv| {
        Err::<(), String>("blocked by policy: destructive git operation".into())
    })
    .unwrap_err();
    assert!(err.contains("blocked by policy"), "got: {err}");

    // The merge is still parked and nothing moved.
    assert_eq!(
        repo_op::detect(repo.path()).unwrap().unwrap().kind,
        OperationKind::Merge
    );
    assert_eq!(repo.head_oid(), head);
    assert_eq!(git_out(repo.path(), &["status", "--porcelain"]), status);
}

/// The gate sees the real line for the operation actually present, which is
/// what makes judging meaningful — a rendered `git rebase --abort` judged
/// against a parked cherry-pick would approve the wrong command.
#[test]
fn the_gate_is_shown_the_argv_that_runs() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["cherry-pick", "side"]);

    let (seen, _) = repo_op::run_action_with(&repo.path_str(), OperationAction::Abort, |argv| {
        Ok(argv.iter().map(|a| a.to_string()).collect::<Vec<_>>())
    })
    .unwrap();
    assert_eq!(seen, vec!["git", "cherry-pick", "--abort"]);
    assert_eq!(repo_op::detect(repo.path()).unwrap(), None);
}

/// The gate is never consulted for an action git would refuse anyway: the
/// availability check runs first, so a policy prompt cannot appear for a
/// button that could not have worked.
#[test]
fn an_unavailable_action_is_refused_before_the_gate_runs() {
    let repo = TestRepo::with_diverged_branches();
    run_git_allow_failure(repo.path(), &["merge", "--no-edit", "side"]);

    let mut judged = false;
    let err = repo_op::run_action_with(&repo.path_str(), OperationAction::Skip, |_argv| {
        judged = true;
        Ok(())
    })
    .unwrap_err();
    assert!(err.contains("skip"), "got: {err}");
    assert!(
        !judged,
        "the gate must not be consulted for an impossible action"
    );
}
