use super::*;
use crate::procguard::LockedSpawn;

/// `git init` + one commit, plus an untracked file on disk.
fn repo_with_tracked_and_untracked() -> tempfile::TempDir {
    let dir = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .current_dir(dir.path())
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@e")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@e")
            .output_locked()
            .expect("git runs");
    };
    run(&["init", "-b", "main"]);
    std::fs::write(dir.path().join("README.md"), "# t\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-m", "seed"]);
    std::fs::write(dir.path().join("fresh.txt"), "new\n").unwrap();
    crate::test_support::trust_repo(dir.path());
    dir
}

/// `with_digest` is additive: leaving it out (every existing caller) keeps the
/// digest, and `false` is the diff viewer's read, which names no version
/// because it never saves.
#[test]
fn get_file_blob_computes_the_digest_unless_the_caller_declines_it() {
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let read = |with_digest: Option<bool>| {
        tauri::async_runtime::block_on(cmd_get_file_blob(
            root.clone(),
            "fresh.txt".into(),
            None,
            with_digest,
        ))
        .expect("blob")
    };
    let default = read(None);
    let explicit = read(Some(true));
    let declined = read(Some(false));
    assert!(default.disk_base.is_some(), "default keeps the digest");
    assert_eq!(default.disk_base, explicit.disk_base);
    assert_eq!(declined.disk_base, None, "declined: no digest pass");
    assert_eq!(declined.text, default.text, "the bytes are the same");
}

/// The Overwrite flag reaches the byte-exact path, and an ordinary save of the
/// same mixed-EOL file is still refused.
#[test]
fn write_file_content_overwrite_replaces_a_mixed_eol_file_byte_exactly() {
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    std::fs::write(dir.path().join("fresh.txt"), b"a\r\nb\nc\r\n").unwrap();
    let write = |overwrite: Option<bool>| {
        tauri::async_runtime::block_on(cmd_write_file_content(
            root.clone(),
            "fresh.txt".into(),
            "mine\nonly\n".into(),
            None,
            overwrite,
        ))
    };
    assert!(write(None).is_err(), "an ordinary save is refused");
    assert!(write(Some(false)).is_err());
    assert_eq!(
        std::fs::read(dir.path().join("fresh.txt")).unwrap(),
        b"a\r\nb\nc\r\n"
    );
    write(Some(true)).expect("overwrite succeeds");
    assert_eq!(
        std::fs::read(dir.path().join("fresh.txt")).unwrap(),
        b"mine\nonly\n"
    );
}

/// The gate must never answer for a root it was never given.
///
/// Pre-fix the caller's string went straight to the sidecar as `root`, so
/// the verdict described whatever the harness made of a path this process
/// had not resolved. The needle is the resolution failure itself: a
/// sidecar that is absent, slow, or unhappy writes its own detail and
/// never this one, so the assertion cannot pass by the harness being down.
#[test]
fn a_policy_check_will_not_judge_a_root_that_is_not_a_repository() {
    let dir = tempfile::TempDir::new().unwrap();
    let verdict = policy_verdict_for(&dir.path().to_string_lossy(), "git status");

    assert!(!verdict.checked, "a root that never resolved was judged");
    assert_eq!(verdict.status, crate::harness::PolicyStatus::Unchecked);
    assert!(
        verdict.detail.contains("Not a Git repository"),
        "the refusal must say why the gate did not run: {:?}",
        verdict.detail
    );
    assert_eq!(
        verdict.target, "git status",
        "the judged target is the command, not the root"
    );
}

/// Root shapes that must not reach a second process at all. Each is
/// refused for a different reason, and each must still say which.
#[test]
fn a_policy_check_refuses_every_root_shape_that_cannot_name_a_checkout() {
    for root in [
        "",
        "relative/path",
        "/no/such/directory/anywhere",
        "/tmp/\u{7}bell",
    ] {
        let verdict = policy_verdict_for(root, "git status");
        assert!(!verdict.checked, "{root:?} was forwarded to the harness");
        assert!(
            !verdict.detail.is_empty(),
            "{root:?} was refused without saying why"
        );
    }
}

/// No-narrowing: resolving the root must not cost a real checkout its
/// verdict. Asserted on the refusal text rather than on `checked`, because
/// whether the sidecar answers here depends on the machine.
#[test]
fn a_policy_check_still_forwards_a_real_checkout() {
    let dir = repo_with_tracked_and_untracked();
    let verdict = policy_verdict_for(&dir.path().to_string_lossy(), "git status");
    assert!(
        !verdict.detail.contains("Not a Git repository"),
        "a real checkout was refused by the resolution step: {:?}",
        verdict.detail
    );
}

#[test]
fn discard_declares_delete_for_the_file_it_will_delete() {
    // `discard_changes` runs `restore` AND `clean -f`. For an untracked
    // path the clean is what acts, and it removes the file — so declaring
    // "modify" asked the policy sidecar to judge, and the ledger to
    // record, a gentler operation than the one that ran.
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();

    assert_eq!(discard_file_op(&root, "README.md"), "modify");
    assert_eq!(discard_file_op(&root, "fresh.txt"), "delete");
}

/// Discard is a user's click, so memory pressure must not shed it: the load
/// governor sheds reactive spawns at critical, and a discard whose git
/// children ran in that class was refused — and its `is_tracked` probe, shed
/// too, declared a tracked file's restore as a delete.
#[test]
fn discard_is_never_shed_under_memory_pressure() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    std::fs::write(dir.path().join("README.md"), "# edited\n").unwrap();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    assert_eq!(
        crate::sysload::shed(),
        crate::sysload::Shed {
            background: true,
            reactive: true
        },
        "the injected governor sheds reactive work"
    );
    // The real command, through `off_thread`, which carries this thread's
    // injection to the pool thread it hops to.
    let result =
        tauri::async_runtime::block_on(cmd_discard_changes(root.clone(), "README.md".to_string()));
    assert!(
        result.is_ok(),
        "discard was refused under pressure: {:?}",
        result.err()
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("README.md")).unwrap(),
        "# t\n",
        "the tracked file was restored"
    );
}

/// The frontend retries a read the gate deferred, on its kind. This is the real
/// command, under real injected pressure, so it also proves the producer's text
/// reaches the seam unwrapped for the read the app makes most.
#[test]
fn a_read_shed_under_memory_pressure_reaches_the_frontend_as_a_deferral() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    let error = tauri::async_runtime::block_on(cmd_get_status(root)).expect_err("the read is shed");
    assert_eq!(
        error.kind,
        crate::ipc_error::IpcErrorKind::DeferredUnderLoad,
        "{error}"
    );
    assert!(
        error.message.contains(" deferred under load after "),
        "{error}"
    );
}

/// The commit graph walks history on a scoped helper thread, so the gate's
/// refusal is written there, not on the command's thread. The read is still
/// asked again: the helper's record is handed back at the join.
#[test]
fn a_commit_graph_walk_shed_on_its_helper_thread_reaches_the_frontend_as_a_deferral() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    let error =
        tauri::async_runtime::block_on(cmd_get_commit_graph(root, 100, None, None, None, None))
            .expect_err("the history walk is shed");
    assert_eq!(
        error.kind,
        crate::ipc_error::IpcErrorKind::DeferredUnderLoad,
        "{error}"
    );
    assert!(
        error.message.contains(" deferred under load after "),
        "{error}"
    );
}

/// The engine wraps some runner failures in its own words (`Blame
/// unavailable: ...`). The read is still asked again: the gate's refusal was
/// recorded on this command's thread.
#[test]
fn a_wrapped_read_shed_under_memory_pressure_still_reaches_the_frontend_as_a_deferral() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    let error =
        tauri::async_runtime::block_on(cmd_get_file_blame(root, "README.md".to_string(), None))
            .expect_err("the read is shed");
    assert_eq!(
        error.kind,
        crate::ipc_error::IpcErrorKind::DeferredUnderLoad,
        "{error}"
    );
}

/// The probe that picks the declared operation runs before the gate, so it
/// carries its own promotion: shed as a deferrable read it failed closed to
/// "delete" for a tracked file that is only being restored.
#[test]
fn discard_probe_declares_modify_under_memory_pressure() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    let unpromoted =
        crate::engine::git_cli::run_command_scope(|| GitReader::is_tracked(&root, "README.md"));
    assert!(
        unpromoted.is_err(),
        "the probe is shed when unpromoted: {unpromoted:?}"
    );
    let declared =
        crate::engine::git_cli::run_command_scope(|| Ok(discard_file_op(&root, "README.md")));
    assert_eq!(declared, Ok("modify"));
}

/// Every file write a person clicks — save, conflict save, move, cleanup —
/// passes the commands' `guard_file`, whose task-binding lookup is git. Under
/// critical pressure that lookup, run as a deferrable read, was shed and the
/// save refused as "scope unavailable"; promoted first, it runs.
#[test]
fn a_clicked_file_write_is_judged_under_memory_pressure() {
    use crate::sysload::{GovernorInput, Level};
    let dir = repo_with_tracked_and_untracked();
    let root = dir.path().to_string_lossy().to_string();
    let _pressed = crate::sysload::inject_governor(GovernorInput::test_pressure(Level::Critical));
    let unpromoted = crate::engine::git_cli::run_command_scope(|| {
        crate::harness::guard_file(&root, "README.md", "modify")
    });
    assert!(
        unpromoted
            .as_ref()
            .err()
            .is_some_and(|e| e.contains("memory pressure")),
        "the lookup this guards against is shed when unpromoted: {unpromoted:?}"
    );
    let clicked =
        crate::engine::git_cli::run_command_scope(|| guard_file(&root, "README.md", "modify"));
    assert!(
        clicked.is_ok(),
        "a save was refused under pressure: {:?}",
        clicked.err()
    );
}

#[test]
fn last_fetch_at_reads_fetch_head_mtime_or_none() {
    let dir = repo_with_tracked_and_untracked();
    let repo = validate_repo(dir.path().to_str().unwrap()).unwrap();
    assert_eq!(last_fetch_at_ms(&repo).unwrap(), None);
    let fetch_head = crate::engine::git_cli::resolve_git_common_dir(&repo)
        .unwrap()
        .join("FETCH_HEAD");
    std::fs::write(&fetch_head, "deadbeef\t\tbranch 'main' of somewhere\n").unwrap();
    let stamp = last_fetch_at_ms(&repo).unwrap().expect("FETCH_HEAD mtime");
    assert!(stamp > 0);
}

#[test]
fn discard_fails_closed_when_the_index_cannot_be_read() {
    // A probe that could not run must not produce the gentler claim.
    assert_eq!(
        discard_file_op("/definitely/not/a/repository", "any.txt"),
        "delete"
    );
}

fn verdict(status: crate::harness::PolicyStatus) -> crate::harness::PolicyVerdict {
    crate::harness::PolicyVerdict {
        status,
        checked: true,
        target: "git x".into(),
        rule: String::new(),
        severity: String::new(),
        reason: String::new(),
        demoted: String::new(),
        task_id: String::new(),
        grant_id: String::new(),
        granted_by: String::new(),
        widened: String::new(),
        degraded: Vec::new(),
        detail: String::new(),
        detail_code: String::new(),
    }
}

#[test]
fn strictest_verdict_ranks_blocked_over_clean_passes() {
    use crate::harness::PolicyStatus::*;
    let allowed = strictest_verdict(verdict(Allowed), verdict(Allowed));
    assert_eq!(allowed.status, Allowed);
    let warned = strictest_verdict(verdict(Allowed), verdict(Warned));
    assert_eq!(warned.status, Warned);
    let demoted = strictest_verdict(verdict(Demoted), verdict(Unchecked));
    assert_eq!(demoted.status, Demoted);
    let blocked = strictest_verdict(verdict(Allowed), verdict(Blocked));
    assert_eq!(blocked.status, Blocked);
}

#[test]
fn verdict_fold_keeps_the_strictest_in_any_order() {
    use crate::harness::PolicyStatus::*;
    for order in [
        [Warned, Allowed, Allowed],
        [Allowed, Warned, Allowed],
        [Allowed, Allowed, Warned],
    ] {
        let fold = VerdictFold::default();
        assert!(VerdictFold::default().finish().is_none());
        for status in order {
            fold.record(verdict(status));
        }
        assert_eq!(fold.finish().unwrap().status, Warned, "{order:?}");
    }
}

fn git_in(dir: &std::path::Path, args: &[&str]) -> bool {
    use crate::procguard::LockedSpawn;
    std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output_locked()
        .unwrap()
        .status
        .success()
}

#[test]
fn conflict_save_surfaces_the_file_verdict_not_only_the_last_command() {
    use crate::harness::PolicyStatus::*;
    let dir = tempfile::tempdir().unwrap();
    let p = dir.path();
    assert!(git_in(p, &["init", "-b", "main"]));
    assert!(git_in(p, &["config", "commit.gpgsign", "false"]));
    std::fs::write(p.join("file"), "base\n").unwrap();
    assert!(git_in(p, &["add", "."]) && git_in(p, &["commit", "-m", "base"]));
    assert!(git_in(p, &["checkout", "-b", "incoming"]));
    std::fs::write(p.join("file"), "theirs\n").unwrap();
    assert!(git_in(p, &["commit", "-am", "theirs"]));
    assert!(git_in(p, &["checkout", "main"]));
    std::fs::write(p.join("file"), "ours\n").unwrap();
    assert!(git_in(p, &["commit", "-am", "ours"]));
    assert!(!git_in(p, &["merge", "incoming"]), "must conflict");

    let repo = p.to_str().unwrap();
    // The hermetic test store, never the developer's real trust record.
    let view = crate::repository_trust::inspect(repo).unwrap();
    crate::repository_trust::grant(repo, &view.identity, true).unwrap();
    let snapshot = conflict_session::snapshot(repo, "file").unwrap();
    let request = ConflictSaveRequest {
        file_path: "file".into(),
        revision: snapshot.revision,
        choice: crate::diff::conflict_session::ConflictFileChoice::Ours,
    };
    // The file check warns; every command check afterwards allows. The
    // old handler discarded the file verdict and kept only the last
    // command's, so this reached the UI as a clean allow.
    let saved = save_conflict_folded(
        repo,
        &request,
        |_, _| Ok(verdict(Warned)),
        |_| Ok(verdict(Allowed)),
    )
    .unwrap();
    assert_eq!(saved.policy.status, Warned);
}

/// Lockstep test for commit_amend_argv against GitWriter::commit_inner's
/// branching: empty-message amend is --amend --no-edit, everything else
/// is -m <msg> with an optional trailing --amend.
#[test]
fn commit_amend_argv_mirrors_commit_inner() {
    assert_eq!(
        commit_amend_argv("feat: thing", false),
        vec!["git", "commit", "-m", "feat: thing"]
    );
    assert_eq!(
        commit_amend_argv("feat: thing", true),
        vec!["git", "commit", "-m", "feat: thing", "--amend"]
    );
    // The Squash/Fixup path amends with no message at all.
    assert_eq!(
        commit_amend_argv("", true),
        vec!["git", "commit", "--amend", "--no-edit"]
    );
}

#[test]
fn quick_commit_gates_add_all_then_commit_minus_m() {
    assert_eq!(GitWriter::QUICK_COMMIT_ADD_ARGV, &["add", "--all"]);
    let add_argv: Vec<&str> = std::iter::once("git")
        .chain(GitWriter::QUICK_COMMIT_ADD_ARGV.iter().copied())
        .collect();
    assert_eq!(add_argv, vec!["git", "add", "--all"]);
    assert_eq!(
        commit_amend_argv("feat: all", false),
        vec!["git", "commit", "-m", "feat: all"]
    );
}

#[test]
fn reworded_message_keeps_body_rewrites_subject() {
    assert_eq!(reworded_message("old\n\nbody", "new"), "new\n\nbody");
    assert_eq!(reworded_message("subject only", "new"), "new");
}

/// Lockstep contract with GitReader::page_count_limit: the probe row this
/// fetch-limit appends must survive the reader's clamp, including at
/// exactly MAX_HISTORY_COMMITS, or has_more dies at the ceiling.
#[test]
fn graph_fetch_limit_yields_the_probe_row_even_at_the_ceiling() {
    let max = GitReader::MAX_HISTORY_COMMITS;
    assert_eq!(
        graph_fetch_limit(max),
        max + 1,
        "probe row must exist at exactly the ceiling"
    );
    assert_eq!(graph_fetch_limit(max + 10), max + 1);
    assert_eq!(graph_fetch_limit(0), 2);
}

fn init_repo_with_branch_commits() -> (tempfile::TempDir, String, String, String) {
    let dir = tempfile::TempDir::new().unwrap();
    let git = |args: &[&str]| {
        let output = std::process::Command::new("git")
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .current_dir(dir.path())
            .output_locked()
            .expect("spawn git");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.path().join("base.txt"), "base\n").unwrap();
    git(&["add", "--", "base.txt"]);
    git(&["commit", "-m", "base"]);
    let base = git(&["rev-parse", "HEAD"]);
    std::fs::write(dir.path().join("a.txt"), "a\n").unwrap();
    git(&["add", "--", "a.txt"]);
    git(&["commit", "-m", "A"]);
    let c_a = git(&["rev-parse", "HEAD"]);
    std::fs::write(dir.path().join("b.txt"), "b\n").unwrap();
    git(&["add", "--", "b.txt"]);
    git(&["commit", "-m", "B"]);
    let c_b = git(&["rev-parse", "HEAD"]);
    crate::test_support::trust_repo(dir.path());
    (dir, base, c_a, c_b)
}

/// Bare git runner for tests that only need a side effect (no output).
fn run_git(cwd: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(args)
        .current_dir(cwd)
        .output_locked()
        .expect("spawn git");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// The planned sequence for pick+squash on a branch mirrors
/// execute_rebase_sequence exactly: detach onto the base, replay the
/// steps (squash picks -n and amends --no-edit), then move the branch
/// and check it back out. The rollback pair is gated proactively.
#[test]
fn rebase_planned_commands_mirror_the_writer_sequence_on_a_branch() {
    let (dir, base, c_a, c_b) = init_repo_with_branch_commits();
    let path = dir.path().to_str().unwrap();
    let steps = vec![
        RebaseStep {
            commit_id: c_a.clone(),
            action: crate::engine::git_writer::RebaseActionKind::Pick,
        },
        RebaseStep {
            commit_id: c_b.clone(),
            action: crate::engine::git_writer::RebaseActionKind::Squash,
        },
    ];
    let planned = rebase_planned_commands(path, &base, &steps).unwrap();
    assert_eq!(
        planned,
        vec![
            vec!["git", "checkout", "--detach", &base],
            vec!["git", "cherry-pick", &c_a],
            vec!["git", "cherry-pick", "-n", &c_b],
            vec!["git", "commit", "--amend", "--no-edit"],
            vec!["git", "cherry-pick", "--abort"],
            vec!["git", "checkout", "-f", "main"],
            vec!["git", "branch", "-f", "main", "HEAD"],
            vec!["git", "checkout", "main"],
        ]
    );
}

/// Detached sessions have no branch to fast-forward: the restore target
/// is the detached HEAD oid and there is no branch -f / checkout tail.
#[test]
fn rebase_planned_commands_detached_session_restores_to_head_oid() {
    let (dir, base, c_a, c_b) = init_repo_with_branch_commits();
    let path = dir.path().to_str().unwrap();
    let head = {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let detached = std::process::Command::new("git")
        .args(["checkout", "-q", "--detach"])
        .current_dir(dir.path())
        .output_locked()
        .unwrap();
    assert!(
        detached.status.success(),
        "detaching failed: {}",
        String::from_utf8_lossy(&detached.stderr)
    );
    let planned = rebase_planned_commands(
        path,
        &base,
        &[
            RebaseStep {
                commit_id: c_a.clone(),
                action: crate::engine::git_writer::RebaseActionKind::Pick,
            },
            RebaseStep {
                commit_id: c_b.clone(),
                action: crate::engine::git_writer::RebaseActionKind::Fixup,
            },
        ],
    )
    .unwrap();
    assert_eq!(
        planned,
        vec![
            vec!["git", "checkout", "--detach", &base],
            vec!["git", "cherry-pick", &c_a],
            vec!["git", "cherry-pick", "-n", &c_b],
            vec!["git", "commit", "--amend", "--no-edit"],
            vec!["git", "cherry-pick", "--abort"],
            vec!["git", "checkout", "-f", &head],
        ]
    );
}

/// A reword step composes its amend message from the original body plus
/// the new subject, so the gate judges the real `-m` payload.
#[test]
fn rebase_planned_commands_reword_composes_the_real_message() {
    let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
    // Rewrite HEAD's message so it has a body worth preserving.
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["commit", "--amend", "-m", "A\n\nbody of A"])
        .current_dir(dir.path())
        .output_locked()
        .unwrap();
    assert!(output.status.success());
    let c_a = {
        let output = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output_locked()
            .unwrap();
        String::from_utf8(output.stdout).unwrap().trim().to_string()
    };
    let planned = rebase_planned_commands(
        dir.path().to_str().unwrap(),
        &base,
        &[RebaseStep {
            commit_id: c_a.clone(),
            action: crate::engine::git_writer::RebaseActionKind::Reword("renamed subject".into()),
        }],
    )
    .unwrap();
    assert_eq!(planned[2][0], "git");
    assert_eq!(planned[2][1], "commit");
    assert_eq!(planned[2][2], "-m");
    assert!(
        planned[2][3].starts_with("renamed subject\n\nbody of A"),
        "reword must keep the original body, got {:?}",
        planned[2][3]
    );
    assert_eq!(planned[2][4], "--amend");
}

#[test]
fn rebase_planned_commands_refuses_empty_steps_and_invalid_oids() {
    let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
    let path = dir.path().to_str().unwrap();
    let err = rebase_planned_commands(path, &base, &[]).unwrap_err();
    assert!(err.contains("empty"));
    let err = rebase_planned_commands(
        path,
        &base,
        &[RebaseStep {
            commit_id: "; rm -rf /".into(),
            action: crate::engine::git_writer::RebaseActionKind::Pick,
        }],
    )
    .unwrap_err();
    assert!(err.contains("Invalid revision"), "{err}");
}

/// The restack upstream resolver finds the fork point (here: where feature
/// branched from main) and refuses invalid refs before anything is judged.
/// The full rebase line is assembled in [`cmd_restack`] from this frozen
/// value under one lock span, so planner and executor cannot drift.
#[test]
fn restack_upstream_resolution_resolves_fork_point() {
    let (dir, base, _c_a, _c_b) = init_repo_with_branch_commits();
    run_git(dir.path(), &["branch", "feature", &base]);
    let canon = validate_repo(dir.path().to_str().unwrap()).unwrap();
    let upstream = GitWriter::prepare_restack(&canon, "feature", "main", None).unwrap();
    assert_eq!(upstream, base, "upstream must resolve to the fork base");
    // Invalid refs are refused before anything is rendered or judged.
    assert!(GitWriter::prepare_restack(&canon, "-evil", "main", None).is_err());
}

/// A cascade names the fork point it read the stack at. It is honoured
/// when it really is an ancestor, and refused — never silently replaced
/// by the computed one — when the stack has moved since it was read.
#[test]
fn restack_honours_a_caller_supplied_fork_point_and_refuses_a_stale_one() {
    let (dir, base, c_a, _c_b) = init_repo_with_branch_commits();
    run_git(dir.path(), &["branch", "feature", &base]);
    let canon = validate_repo(dir.path().to_str().unwrap()).unwrap();

    // `base` is genuinely an ancestor of `feature`, so it is taken as-is.
    let honoured =
        GitWriter::prepare_restack(&canon, "feature", "main", Some(base.as_str())).unwrap();
    assert_eq!(honoured, base);

    // `c_a` sits on the other branch, so it is not an ancestor of
    // `feature`: the plan the caller built no longer describes this
    // repository and the restack must not proceed on a widened range.
    let err = GitWriter::prepare_restack(&canon, "feature", "main", Some(c_a.as_str()))
        .expect_err("a non-ancestor fork point must be refused");
    assert!(err.contains("no longer the stack on disk"), "{err}");

    // Anything that is not an object id is refused before it reaches argv.
    assert!(GitWriter::prepare_restack(&canon, "feature", "main", Some("--exec=boom")).is_err());
}

#[test]
fn unborn_head_errors_are_notices_not_faults() {
    use crate::engine::git_reader::is_unborn_head_error;
    // The `git log` phrases are the same classifier's: one table for both readers.
    assert!(is_unborn_head_error(
        "fatal: your current branch 'main' does not have any commits yet"
    ));
    assert!(is_unborn_head_error("fatal: bad default revision 'HEAD'"));
    assert!(is_unborn_head_error(
        "fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree."
    ));
    assert!(is_unborn_head_error("fatal: Needed a single revision"));
    assert!(is_unborn_head_error(
        "fatal: ambiguous argument 'HEAD': unknown revision or path not in the working tree.\nUse '--' to separate paths from revisions"
    ));
    assert!(!is_unborn_head_error("fatal: bad object HEAD"));
    assert!(!is_unborn_head_error("fatal: not a git repository"));
}

/// Every read failing used to leave every part absent, and the summary
/// then said "Worktree clean" about a worktree nothing had looked at.
#[test]
fn a_worktree_summary_whose_reads_failed_is_an_error_not_clean() {
    let repo = crate::test_support::git_repo();
    crate::test_support::git_in(repo.path(), &["commit", "--allow-empty", "-m", "seed"]);
    let path = repo.path().to_str().unwrap();
    let read = super::worktree_ai_summary(path, path, Some("main")).expect("summary");
    assert!(read.contains("HEAD±: clean"), "{read}");
    let failed = crate::engine::git_cli::with_forced_spawn_failure(|| {
        super::worktree_ai_summary(path, path, Some("main"))
    });
    let error = failed.expect_err("no read answered, so there is no summary");
    assert!(
        error.contains("forced by test"),
        "the cause is named: {error}"
    );
}

/// The Work view asks for up to 32 task titles per refresh. Each title
/// was its own command, and each command authenticated the repository
/// with `git worktree list`, so one refresh started a git process per
/// title.
#[test]
fn many_task_scopes_authenticate_the_repository_once() {
    let dir = crate::test_support::git_repo();
    let repo = dir.path().canonicalize().expect("canonical repository");
    let path = repo.to_str().expect("utf8");
    let db = crate::tasks::store_path(path);
    std::fs::create_dir_all(db.parent().unwrap()).unwrap();
    let store = dc_store::Store::open(&db).unwrap();
    for id in ["T1", "T2", "T3"] {
        store
            .connection()
            .execute(
                &format!(
                    "INSERT INTO tasks (id, title, description, planned_files_json, status)
                     VALUES ('{id}', 'title of {id}', '', '[]', 'in_progress')"
                ),
                [],
            )
            .unwrap();
    }
    let listings = || {
        crate::engine::git_cli::spawn_log::spawns_in(&repo)
            .iter()
            .filter(|argv| argv.iter().any(|arg| arg == "worktree"))
            .count()
    };
    let ids: Vec<String> = ["T1", "T2", "T3", "T2", "MISSING"]
        .iter()
        .map(|id| id.to_string())
        .collect();

    let before = listings();
    let scopes = super::task_scopes(path, &ids).expect("scopes");
    assert_eq!(listings() - before, 1, "one authentication for the set");
    let titles: Vec<&str> = scopes.iter().map(|scope| scope.title.as_str()).collect();
    assert_eq!(titles, ["title of T1", "title of T2", "title of T3"]);

    let before = listings();
    assert!(super::task_scopes(path, &[]).expect("empty").is_empty());
    assert_eq!(listings(), before, "nothing asked, nothing spawned");

    let too_many: Vec<String> = (0..=crate::tasks::MAX_SCOPE_IDS)
        .map(|n| format!("T{n}"))
        .collect();
    let error = super::task_scopes(path, &too_many).expect_err("over the limit");
    assert!(error.contains("the limit is 64"), "{error}");
}

#[test]
fn an_empty_repository_graph_is_a_notice_not_a_fault() {
    let repo = crate::test_support::git_repo();
    let payload = tauri::async_runtime::block_on(cmd_get_commit_graph(
        repo.path().to_string_lossy().into_owned(),
        100,
        None,
        None,
        None,
        None,
    ))
    .expect("empty repository graph must load");
    assert!(payload.rows.is_empty(), "no commits to draw");
    assert!(payload.head_id.is_none());
    assert!(
        payload
            .warnings
            .iter()
            .all(|w| !w.to_ascii_lowercase().contains("head unavailable")),
        "unborn HEAD must not be a diagnostics fault: {:?}",
        payload.warnings
    );
    assert!(
        payload.notices.iter().any(|n| n.contains("no commits yet")),
        "unborn HEAD must be disclosed as a notice: {:?}",
        payload.notices
    );
}

/// A repository that has commits but whose HEAD branch ref is unreadable is a
/// fault, not a repository fact: git words it exactly like an unborn branch
/// ("ambiguous argument 'HEAD'"), so the text alone cannot be the evidence.
#[test]
fn an_unreadable_head_in_a_repository_with_commits_is_a_warning_not_no_commits() {
    let repo = crate::test_support::git_repo();
    crate::test_support::git_in(repo.path(), &["commit", "--allow-empty", "-m", "seed"]);
    let branch = std::fs::read_to_string(repo.path().join(".git/HEAD")).expect("HEAD");
    let branch_ref = branch.trim().strip_prefix("ref: ").expect("symbolic HEAD");
    std::fs::write(
        repo.path().join(".git").join(branch_ref),
        "not an object id\n",
    )
    .expect("corrupt the branch ref");
    let payload = tauri::async_runtime::block_on(cmd_get_commit_graph(
        repo.path().to_string_lossy().into_owned(),
        100,
        None,
        None,
        None,
        None,
    ))
    .expect("a degraded HEAD must not fail the load");
    assert!(
        payload
            .notices
            .iter()
            .all(|n| !n.contains("no commits yet")),
        "a broken HEAD must not be stated as an empty repository: {:?}",
        payload.notices
    );
    assert!(
        payload
            .warnings
            .iter()
            .any(|w| w.contains("HEAD unavailable")),
        "the unreadable HEAD is a diagnosable fault: {:?}",
        payload.warnings
    );
}

/// An orphan checkout in a repository that already has commits really is
/// unborn: the notice is true there, and must stay a notice.
#[test]
fn an_orphan_branch_in_a_repository_with_commits_is_still_unborn() {
    let repo = crate::test_support::git_repo();
    crate::test_support::git_in(repo.path(), &["commit", "--allow-empty", "-m", "seed"]);
    crate::test_support::git_in(repo.path(), &["checkout", "--orphan", "fresh"]);
    let payload = tauri::async_runtime::block_on(cmd_get_commit_graph(
        repo.path().to_string_lossy().into_owned(),
        100,
        None,
        None,
        None,
        None,
    ))
    .expect("orphan graph must load");
    assert!(
        payload.notices.iter().any(|n| n.contains("no commits yet")),
        "{:?}",
        payload.notices
    );
    assert!(payload
        .warnings
        .iter()
        .all(|w| !w.contains("HEAD unavailable")));
}

/// The payload carries assembly warnings through a serde round trip when
/// present, and a payload from before the field existed still deserializes
/// to an empty vec instead of failing (the `#[serde(default)]` contract).
#[test]
fn commit_graph_payload_round_trips_warnings() {
    let warning = "HEAD unavailable (bad object HEAD); commit graph may lack the HEAD marker";
    let notice = "36 commit(s) reachable only from refs outside branches, remotes and tags \
                  are not drawn.";
    let payload = CommitGraphPayload {
        rows: Vec::new(),
        head_id: None,
        refs: Vec::new(),
        has_more: false,
        warnings: vec![warning.to_string()],
        notices: vec![notice.to_string()],
        mainline_id: Some("abc".to_string()),
        mainline_name: Some("main".to_string()),
    };
    let json = serde_json::to_string(&payload).unwrap();
    assert!(
        json.contains(&format!(r#""warnings":["{warning}"]"#)),
        "warnings must serialize verbatim, got {json}"
    );
    let back: CommitGraphPayload = serde_json::from_str(&json).unwrap();
    assert_eq!(back.warnings, vec![warning]);
    // Two lists on the wire, not one: a client that folded them back
    // together would refile every disclosure as a fault again.
    assert_eq!(back.notices, vec![notice]);

    // `folds` was shipped (and never read) by earlier builds; a payload
    // carrying it must still deserialize.
    let legacy = r#"{"rows":[],"folds":[],"head_id":null,"refs":[],"has_more":false}"#;
    let back: CommitGraphPayload = serde_json::from_str(legacy).unwrap();
    assert!(back.warnings.is_empty(), "absent field defaults to empty");
    assert!(back.notices.is_empty(), "absent field defaults to empty");
    assert!(back.mainline_id.is_none() && back.mainline_name.is_none());
}

fn decoration(name: &str, kind: RefKind, commit_id: &str, is_head: bool) -> RefDecoration {
    RefDecoration {
        remote: None,
        name: name.to_string(),
        kind,
        commit_id: commit_id.to_string(),
        is_head,
    }
}

/// The default branch supplies every tip carrying its name, local first,
/// all labelled with the local name; HEAD is the fallback anchor.
#[test]
fn mainline_hint_takes_the_default_branch_local_then_remotes() {
    let refs = vec![
        decoration("feature", RefKind::Local, "f1", true),
        decoration("main", RefKind::Local, "m1", false),
        decoration("origin/main", RefKind::Remote, "o1", false),
        decoration("upstream/main", RefKind::Remote, "u1", false),
        decoration("origin/feature", RefKind::Remote, "f1", false),
        decoration("v1", RefKind::Tag, "m1", false),
    ];
    let resolved = resolve_mainline_hint(&refs, Some("main"), Some("f1"));
    assert_eq!(
        resolved.hint,
        MainlineHint {
            branch_tips: vec!["m1".into(), "o1".into(), "u1".into()],
            fallback_tip: Some("f1".into()),
        }
    );
    assert_eq!(resolved.name_for("m1", &refs).as_deref(), Some("main"));
    assert_eq!(
        resolved.name_for("o1", &refs).as_deref(),
        Some("main"),
        "a remote copy of a local branch is still labelled by the branch"
    );
    assert_eq!(
        resolved.name_for("f1", &refs).as_deref(),
        Some("feature"),
        "the HEAD fallback is labelled with the checked-out branch"
    );
    assert_eq!(resolved.name_for("nowhere", &refs), None);
}

/// Without a resolved default branch the conventional names are probed
/// in order; a remote-only branch is labelled by its remote ref.
#[test]
fn mainline_hint_falls_back_to_conventional_names_and_remote_only_labels() {
    let refs = vec![
        decoration("origin/master", RefKind::Remote, "r1", false),
        decoration("develop", RefKind::Local, "d1", false),
    ];
    let resolved = resolve_mainline_hint(&refs, None, None);
    assert_eq!(resolved.hint.branch_tips, vec!["r1".to_string()]);
    assert_eq!(resolved.hint.fallback_tip, None);
    assert_eq!(
        resolved.name_for("r1", &refs).as_deref(),
        Some("origin/master")
    );

    // A default branch that has no refs at all must NOT fall through to
    // another name: pinning `develop` when the repository's default is
    // `release` would straighten the wrong branch.
    let resolved = resolve_mainline_hint(&refs, Some("release"), Some("h"));
    assert_eq!(resolved.hint.branch_tips, vec!["r1".to_string()]);
    assert_eq!(
        resolve_mainline_hint(&[], Some("release"), Some("h")).hint,
        MainlineHint {
            branch_tips: Vec::new(),
            fallback_tip: Some("h".into()),
        }
    );
}

/// A detached HEAD is labelled `HEAD`; blank ids never become tips.
#[test]
fn mainline_hint_detached_head_and_blank_ids() {
    let refs = vec![decoration("HEAD", RefKind::Head, "h1", true)];
    let resolved = resolve_mainline_hint(&refs, Some(""), Some("h1"));
    assert!(resolved.hint.branch_tips.is_empty());
    assert_eq!(resolved.hint.fallback_tip.as_deref(), Some("h1"));
    assert_eq!(resolved.name_for("h1", &refs).as_deref(), Some("HEAD"));
    assert_eq!(
        resolve_mainline_hint(&refs, None, Some("  "))
            .hint
            .fallback_tip,
        None
    );
}

/// Children whose argv names `subcommand`, started in any of `dirs`.
fn spawns_naming(dirs: &[std::path::PathBuf], subcommand: &str) -> usize {
    dirs.iter()
        .map(|dir| {
            crate::engine::git_cli::spawn_log::spawns_in(dir)
                .iter()
                .filter(|argv| argv.iter().any(|arg| arg == subcommand))
                .count()
        })
        .sum()
}

/// The main checkout and every linked worktree of a fixture, in the spelling
/// git lists them in (which is what the spawn log is keyed by).
fn every_checkout(
    main: &std::path::Path,
    linked: &[std::path::PathBuf],
) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![crate::engine::git_cli::canonicalize_plain(main).unwrap()];
    dirs.extend(linked.iter().cloned());
    dirs
}

/// A listing that does not say how deep to measure is the cheap one. The
/// default used to be the full scan — status, diff, divergence and routes
/// for every worktree — so a caller that forgot to ask paid for all of it:
/// 18 worktrees cost 37 git children per listing, through a spawn budget of
/// 8 a second.
#[test]
fn the_default_worktree_listing_spawns_no_measurement() {
    let (main, _parent, linked) = crate::test_support::repo_with_worktrees(17);
    let dirs = every_checkout(main.path(), &linked);
    let before = (
        spawns_naming(&dirs, "status"),
        spawns_naming(&dirs, "diff"),
        spawns_naming(&dirs, "rev-list"),
    );
    let listed = tauri::async_runtime::block_on(cmd_list_worktrees(
        main.path().to_string_lossy().into_owned(),
        None,
    ))
    .expect("listing");
    assert_eq!(listed.len(), 18);
    let status = spawns_naming(&dirs, "status") - before.0;
    let diff = spawns_naming(&dirs, "diff") - before.1;
    let rev_list = spawns_naming(&dirs, "rev-list") - before.2;
    assert!(status <= 18, "{status} status children for 18 worktrees");
    assert_eq!(diff, 0, "the default listing diffed {diff} worktrees");
    assert_eq!(rev_list, 0, "the default listing measured divergence");
}

/// The panel that renders diff stats, divergence and routes asks for them,
/// and every row still carries them.
#[test]
fn a_full_worktree_listing_still_measures_every_row() {
    let (main, _parent, linked) = crate::test_support::repo_with_worktrees(3);
    std::fs::write(linked[2].join("seed.txt"), "seed\nchanged\n").unwrap();
    let listed = tauri::async_runtime::block_on(cmd_list_worktrees(
        main.path().to_string_lossy().into_owned(),
        Some(crate::engine::worktree::ScanDepth::Full),
    ))
    .expect("listing");
    assert_eq!(listed.len(), 4);
    for row in &listed {
        assert_eq!(row.scan_note, None, "{}", row.path);
        assert!(row.dirty_files.is_some(), "{}", row.path);
        assert!(row.diff_stat.is_some(), "{}", row.path);
        assert!(row.main_divergence.is_some(), "{}", row.path);
    }
    let changed = listed
        .iter()
        .find(|row| std::path::Path::new(&row.path) == linked[2])
        .expect("listed");
    assert_eq!(changed.dirty_files, Some(1));
    assert_eq!(changed.diff_stat.as_ref().map(|s| s.files_changed), Some(1));
}

/// Timing of the worktree listing at each depth on a real repository, for
/// the record a change to the scan cites. Ignored: it reads `PROFILE_REPO`.
///
/// `PROFILE_REPO=/path/to/repo cargo test --manifest-path src-tauri/Cargo.toml \
///   worktree_listing_profile -- --ignored --nocapture`
///
/// Warm (one unmeasured listing first), minimum of five. A unit-test build
/// has no spawn-rate budget, so the shipped app additionally waits about
/// 125 ms for every child past its burst; the child counts printed are what
/// that cost scales with.
#[test]
#[ignore]
fn worktree_listing_profile() {
    let named = std::env::var("PROFILE_REPO").expect("PROFILE_REPO names a repository");
    // Relative to the crate directory, or through a symlink, either is fine.
    let repo = std::fs::canonicalize(&named)
        .expect("PROFILE_REPO exists")
        .to_string_lossy()
        .into_owned();
    crate::test_support::trust_repo(std::path::Path::new(&repo));
    let depths = [
        ("default", None),
        ("listing", Some(crate::engine::worktree::ScanDepth::Listing)),
        ("dirty", Some(crate::engine::worktree::ScanDepth::Dirty)),
        ("full", Some(crate::engine::worktree::ScanDepth::Full)),
    ];
    for (label, depth) in depths {
        let list = || {
            tauri::async_runtime::block_on(cmd_list_worktrees(repo.clone(), depth))
                .expect("listing")
        };
        let warm = list();
        let dirs: Vec<std::path::PathBuf> = warm
            .iter()
            .map(|row| std::path::PathBuf::from(&row.path))
            .collect();
        let mut best = std::time::Duration::MAX;
        let mut children = 0;
        for _ in 0..5 {
            let before: usize = dirs
                .iter()
                .map(|dir| crate::engine::git_cli::spawn_log::spawns_in(dir).len())
                .sum();
            let started = std::time::Instant::now();
            list();
            best = best.min(started.elapsed());
            let after: usize = dirs
                .iter()
                .map(|dir| crate::engine::git_cli::spawn_log::spawns_in(dir).len())
                .sum();
            children = after - before;
        }
        eprintln!(
            "worktree listing {label}: min {} ms of 5 over {} worktrees, {children} git children",
            best.as_millis(),
            warm.len()
        );
    }
}

/// A main checkout whose shared `info/exclude` ignores `node_modules/`, which
/// holds one package, and one linked worktree.
fn checkout_with_an_ignored_cache() -> (tempfile::TempDir, tempfile::TempDir, std::path::PathBuf) {
    let (main, parent, linked) = crate::test_support::repo_with_worktrees(1);
    std::fs::create_dir_all(main.path().join(".git/info")).unwrap();
    std::fs::write(main.path().join(".git/info/exclude"), "node_modules/\n").unwrap();
    std::fs::create_dir_all(main.path().join("node_modules/pkg")).unwrap();
    std::fs::write(main.path().join("node_modules/pkg/index.js"), "x").unwrap();
    (main, parent, linked.into_iter().next().unwrap())
}

/// The target of a cache sync is whatever path the webview sent. Anything
/// that is not a worktree of the repository is refused before a byte is
/// copied: an unrelated directory, another repository, a directory inside a
/// worktree.
#[test]
fn a_cache_sync_into_anything_but_a_worktree_of_the_repository_is_refused() {
    let (main, _parent, linked) = checkout_with_an_ignored_cache();
    let repo = main.path().to_string_lossy().into_owned();
    let elsewhere = tempfile::TempDir::new().unwrap();
    let other_repository = crate::test_support::git_repo();
    let inside = linked.join("sub");
    std::fs::create_dir_all(&inside).unwrap();
    for target in [elsewhere.path(), other_repository.path(), inside.as_path()] {
        let before = std::fs::read_dir(target).unwrap().count();
        let refused = tauri::async_runtime::block_on(cmd_worktree_cow_sync(
            repo.clone(),
            target.to_string_lossy().into_owned(),
        ));
        assert!(refused.is_err(), "{}: {refused:?}", target.display());
        assert_eq!(
            std::fs::read_dir(target).unwrap().count(),
            before,
            "{} gained entries",
            target.display()
        );
    }
}

/// A linked worktree of the repository still receives the caches, and the
/// membership check reads the worktree list without scanning any worktree.
#[test]
fn a_cache_sync_into_a_linked_worktree_copies_and_scans_nothing() {
    let (main, _parent, linked) = checkout_with_an_ignored_cache();
    let dirs = every_checkout(main.path(), std::slice::from_ref(&linked));
    let status_before = spawns_naming(&dirs, "status");
    let copied = tauri::async_runtime::block_on(cmd_worktree_cow_sync(
        main.path().to_string_lossy().into_owned(),
        linked.to_string_lossy().into_owned(),
    ))
    .expect("synced");
    assert_eq!(copied.len(), 1, "{copied:?}");
    assert!(linked.join("node_modules/pkg/index.js").is_file());
    assert_eq!(spawns_naming(&dirs, "status"), status_before);
}

/// Background work (the live index, a docs refresh from a watcher) was
/// admitted to the command runner like any other command, so a repository
/// burst could put every blocking-pool thread to sleep on background child
/// processes. At most two of them hold a pool thread at once; the rest wait
/// without one.
#[test]
fn background_commands_hold_at_most_two_pool_threads() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    let running = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let finished = Arc::new(AtomicUsize::new(0));
    let jobs: Vec<_> = (0..5)
        .map(|_| {
            let (running, peak, finished) = (running.clone(), peak.clone(), finished.clone());
            tauri::async_runtime::spawn(off_thread_background(move || {
                let now = running.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(150));
                running.fetch_sub(1, Ordering::SeqCst);
                finished.fetch_add(1, Ordering::SeqCst);
                Ok(())
            }))
        })
        .collect();
    for job in jobs {
        tauri::async_runtime::block_on(job)
            .expect("joined")
            .expect("job result");
    }
    assert_eq!(finished.load(Ordering::SeqCst), 5, "every job still runs");
    assert_eq!(
        peak.load(Ordering::SeqCst),
        2,
        "background jobs overlapped beyond the limit"
    );
}

/// A third background job waits for a permit rather than a thread.
#[test]
fn a_third_background_job_waits_until_one_of_the_first_two_ends() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    let release = Arc::new(AtomicBool::new(false));
    let third_started = Arc::new(AtomicBool::new(false));
    let hold = |release: Arc<AtomicBool>| {
        tauri::async_runtime::spawn(off_thread_background(move || {
            while !release.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(())
        }))
    };
    let first = hold(release.clone());
    let second = hold(release.clone());
    std::thread::sleep(std::time::Duration::from_millis(100));
    let flag = third_started.clone();
    let third = tauri::async_runtime::spawn(off_thread_background(move || {
        flag.store(true, Ordering::SeqCst);
        Ok(())
    }));
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !third_started.load(Ordering::SeqCst),
        "a third background job ran while two were in flight"
    );
    release.store(true, Ordering::SeqCst);
    for job in [first, second, third] {
        tauri::async_runtime::block_on(job)
            .expect("joined")
            .expect("job result");
    }
    assert!(third_started.load(Ordering::SeqCst));
}

// ---------------------------------------------------------------------------
// Stack refresh: the repair pass for branches beyond the history window
// ---------------------------------------------------------------------------

/// A repository whose `main` is `commits` linear empty commits, with a branch
/// at each `(name, index)` and, when `orphan` is set, a branch `orphan` that
/// shares no history with `main`. Written by one `git fast-import`, so a
/// fixture of hundreds of commits costs one spawn.
fn stack_chain(
    commits: usize,
    branches: &[(String, usize)],
    orphan: bool,
) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::io::Write;
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    let mut stream = String::new();
    let commit = |stream: &mut String, refname: &str, mark: usize, from: Option<usize>| {
        let message = format!("c{mark}\n");
        stream.push_str(&format!("commit refs/heads/{refname}\nmark :{mark}\n"));
        stream.push_str(&format!(
            "committer T <t@t> {} +0000\ndata {}\n{message}",
            1_700_000_000 + mark,
            message.len()
        ));
        if let Some(from) = from {
            stream.push_str(&format!("from :{from}\n"));
        }
        stream.push('\n');
    };
    for n in 1..=commits {
        commit(&mut stream, "main", n, (n > 1).then(|| n - 1));
    }
    if orphan {
        commit(&mut stream, "orphan", commits + 1, None);
        commit(&mut stream, "orphan", commits + 2, Some(commits + 1));
    }
    for (name, index) in branches {
        stream.push_str(&format!("reset refs/heads/{name}\nfrom :{}\n\n", index + 1));
    }
    let mut child = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "fast-import: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    crate::test_support::trust_repo(&repo);
    (dir, repo)
}

/// Twenty branches `b00`..`b19` at main commits 10, 13, ... 67: far below a
/// 60-commit window over 150 commits, each sitting on the one under it.
fn twenty_deep_branches() -> Vec<(String, usize)> {
    (0..20).map(|i| (format!("b{i:02}"), 10 + 3 * i)).collect()
}

/// Rows of the spawn log for `repo` from `since` on.
fn spawns_since(repo: &std::path::Path, since: usize) -> Vec<Vec<String>> {
    crate::engine::git_cli::spawn_log::spawns_in(repo)[since..].to_vec()
}

fn first_parent_walks(spawns: &[Vec<String>]) -> usize {
    spawns
        .iter()
        .filter(|argv| {
            argv.iter().any(|a| a == "rev-list") && argv.iter().any(|a| a == "--first-parent")
        })
        .count()
}

fn parent_of(payload: &crate::stack::StackHierarchyPayload, branch: &str) -> Option<String> {
    payload
        .nodes
        .iter()
        .find(|n| n.branch_name == branch)
        .unwrap_or_else(|| panic!("no node for {branch}"))
        .parent_branch_name
        .clone()
}

const FIXTURE_WINDOW: usize = 60;

/// S4-03: every branch the window cannot place used to get its own
/// `rev-list --first-parent` walk of up to 100,000 commits.
#[test]
fn a_stack_refresh_repairs_twenty_branches_beyond_the_window_in_one_walk() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    let before = crate::engine::git_cli::spawn_log::spawns_in(&repo).len();
    let payload = crate::engine::ref_cache::eager(|| {
        stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION)
    })
    .expect("stack loads");
    let spawned = spawns_since(&repo, before);
    assert_eq!(
        first_parent_walks(&spawned),
        1,
        "one shared pass, not one per branch: {spawned:#?}"
    );
    assert_eq!(parent_of(&payload, "b00"), None, "nothing is below b00");
    for i in 1..20 {
        assert_eq!(
            parent_of(&payload, &format!("b{i:02}")),
            Some(format!("b{:02}", i - 1)),
            "b{i:02} sits on the previous branch"
        );
    }
    assert_eq!(
        payload.unresolved_branches, 0,
        "b00 reached the root: settled"
    );
    assert!(!payload.walk_incomplete);
}

/// S4-03: with no ref movement the next tick is answered from the kept
/// answers: no process at all.
#[test]
fn a_stack_refresh_with_no_ref_movement_spawns_nothing() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    crate::engine::ref_cache::eager(|| {
        let first =
            stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION).expect("first tick");
        let before = crate::engine::git_cli::spawn_log::spawns_in(&repo).len();
        let second = stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION)
            .expect("second tick");
        let spawned = spawns_since(&repo, before);
        assert!(
            spawned.is_empty(),
            "an idle repository was walked again: {spawned:#?}"
        );
        assert_eq!(first, second);
    });
}

/// A tick after a ref moved is answered from git again, and the new base shows.
#[test]
fn a_ref_that_moves_between_ticks_is_walked_again() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    crate::engine::ref_cache::eager(|| {
        let first =
            stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION).expect("first tick");
        assert_eq!(parent_of(&first, "b00"), None);
        // b05 moves below b00: b00 now sits on it.
        let target = crate::engine::git_cli::git_text(&repo, &["rev-parse", "main~145"]).unwrap();
        run_git(&repo, &["update-ref", "refs/heads/b05", target.trim()]);
        let before = crate::engine::git_cli::spawn_log::spawns_in(&repo).len();
        let second = stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION)
            .expect("second tick");
        assert_eq!(parent_of(&second, "b00"), Some("b05".to_string()));
        let spawned = spawns_since(&repo, before);
        assert_eq!(
            first_parent_walks(&spawned),
            1,
            "a moved ref must not be answered from the old walk"
        );
        // The card's bound: the history walks a refresh over twenty unresolved
        // branches starts are the window's `log` and the one pass (was
        // 1 + 20). The branch listing and its default-branch probes are
        // other spawns, unchanged by this card, and are not counted here.
        let history_walks = spawned
            .iter()
            .filter(|argv| argv.iter().any(|a| a == "log" || a == "rev-list"))
            .count();
        assert!(
            history_walks <= 3,
            "a refresh after a ref moved started {history_walks} history walks: {spawned:#?}"
        );
    });
}

/// A pass that hits its commit cap labels what it could not settle: the
/// branches it did place stay placed, the rest are counted, and the payload
/// says the walk was cut short.
#[test]
fn a_walk_that_exhausts_its_budget_counts_what_it_left_unresolved() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    let budget = StackWalkBudget {
        max_commits: 5,
        ..StackWalkBudget::PRODUCTION
    };
    let payload = crate::engine::ref_cache::eager(|| stack_hierarchy(path, FIXTURE_WINDOW, budget))
        .expect("a partial walk is an answer, not a failure");
    assert!(payload.walk_incomplete, "the cap was hit");
    // The five newest commits are b19's tip down to b18's: b19 is placed by
    // the shared pass. The rest used to be left unresolved because the SHARED
    // cap ran out (19); the per-branch walks this replaced each had their own
    // cap, and b01..b18 each sit three commits above their base, so each is
    // placed by its own pass. Only b00 (ten commits from the root) really
    // exceeds a 5-commit cap, and is not reported as a settled root.
    assert_eq!(parent_of(&payload, "b19"), Some("b18".to_string()));
    assert_eq!(parent_of(&payload, "b10"), Some("b09".to_string()));
    assert_eq!(parent_of(&payload, "b01"), Some("b00".to_string()));
    assert_eq!(
        payload.unresolved_branches, 1,
        "only b00 is beyond the cap; b00 is not reported as a settled root"
    );
    assert_eq!(payload.nodes.len(), 21, "every branch is still listed");
}

/// A deadline that expires before the pass finishes leaves every candidate
/// unresolved and says so; the rest of the payload still loads.
#[test]
fn a_walk_whose_deadline_expires_is_labelled_not_failed() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    let budget = StackWalkBudget {
        deadline: std::time::Duration::ZERO,
        ..StackWalkBudget::PRODUCTION
    };
    let payload = stack_hierarchy(path, FIXTURE_WINDOW, budget)
        .expect("a deadline in the repair pass must not fail the load");
    assert!(payload.walk_incomplete);
    assert_eq!(payload.unresolved_branches, 20);
    assert_eq!(payload.nodes.len(), 21);
}

/// Shapes that need no pass, or whose pass has nothing to find.
#[test]
fn stack_shapes_that_need_no_walk_spawn_none_and_report_nothing_unresolved() {
    let _held = crate::engine::ref_cache::retention();
    // No branches besides main.
    let (_a, repo_a) = stack_chain(30, &[], false);
    // A branch on main's tip is placed from the window alone.
    let (_b, repo_b) = stack_chain(30, &[("twin".to_string(), 29)], false);
    for (repo, nodes) in [(&repo_a, 1), (&repo_b, 2)] {
        let before = crate::engine::git_cli::spawn_log::spawns_in(repo).len();
        let payload = crate::engine::ref_cache::eager(|| {
            stack_hierarchy(
                repo.to_str().unwrap(),
                FIXTURE_WINDOW,
                StackWalkBudget::PRODUCTION,
            )
        })
        .expect("stack loads");
        assert_eq!(payload.nodes.len(), nodes);
        assert_eq!(
            first_parent_walks(&spawns_since(repo, before)),
            0,
            "nothing to repair, nothing walked"
        );
        assert_eq!(payload.unresolved_branches, 0);
        assert!(!payload.walk_incomplete);
    }
}

/// A branch with no common history reaches its own root without meeting a
/// tip: a settled, honest root, not an unresolved one.
#[test]
fn a_branch_with_no_common_history_is_a_settled_root() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &[("b00".to_string(), 10)], true);
    let payload = crate::engine::ref_cache::eager(|| {
        stack_hierarchy(
            repo.to_str().unwrap(),
            FIXTURE_WINDOW,
            StackWalkBudget::PRODUCTION,
        )
    })
    .expect("stack loads");
    assert_eq!(parent_of(&payload, "orphan"), None);
    assert_eq!(parent_of(&payload, "b00"), None);
    assert_eq!(payload.unresolved_branches, 0);
    assert!(!payload.walk_incomplete);
}

/// A detached HEAD has no current branch; the repair pass and the breadcrumb
/// must not depend on one.
#[test]
fn a_detached_head_still_repairs_the_stack() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    run_git(&repo, &["checkout", "-q", "--detach", "main~3"]);
    let payload = stack_hierarchy(
        repo.to_str().unwrap(),
        FIXTURE_WINDOW,
        StackWalkBudget::PRODUCTION,
    )
    .expect("stack loads on a detached HEAD");
    assert_eq!(payload.default_branch, "main");
    assert_eq!(parent_of(&payload, "b07"), Some("b06".to_string()));
    assert_eq!(payload.unresolved_branches, 0);
}

/// S4-04 at the command's own arithmetic: the page the graph asks for at a
/// cap that equals one history chunk (the default 5,000 over a 5,000-row
/// chunk) is one `git log`, probe row included.
#[test]
fn the_default_graph_page_is_one_history_walk() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(40, &[], false);
    let path = repo.to_str().unwrap();
    let chunk = 10;
    let before = crate::engine::git_cli::spawn_log::spawns_in(&repo).len();
    let rows = crate::engine::git_reader::with_history_chunk(chunk, || {
        crate::engine::ref_cache::eager(|| {
            GitReader::read_commit_history_paged(
                path,
                0,
                graph_fetch_limit(chunk),
                None,
                None,
                RefScope::Named,
            )
        })
    })
    .expect("page loads");
    assert_eq!(
        rows.len(),
        chunk + 1,
        "the page plus the has_more probe row"
    );
    let logs = spawns_since(&repo, before)
        .iter()
        .filter(|argv| argv.iter().any(|a| a == "log"))
        .count();
    assert_eq!(logs, 1, "cap + 1 rows over a chunk of cap rows");
}

// ---------------------------------------------------------------------------
// Adversarial: the shared first-parent pass against the per-branch walks it
// replaced (S4-03). The oracle below is the old `first_parent_chain` and its
// caller, reconstructed from cd52ff3c.
// ---------------------------------------------------------------------------

/// A repository from an explicit DAG. Commit `i + 1` has `parents[i]` (1-based
/// marks, first entry the first parent) and committer date `dates[i]`; every
/// `(name, mark)` in `refs` becomes `refs/heads/name`. One `git fast-import`.
fn dag_repo(
    parents: &[Vec<usize>],
    dates: &[i64],
    refs: &[(String, usize)],
) -> (tempfile::TempDir, std::path::PathBuf) {
    use std::io::Write;
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    run_git(&repo, &["init", "-q", "-b", "main"]);
    let mut stream = String::new();
    for (i, ps) in parents.iter().enumerate() {
        let mark = i + 1;
        let message = format!("c{mark}\n");
        if ps.is_empty() {
            stream.push_str("reset refs/heads/tmp\n\n");
        }
        stream.push_str(&format!("commit refs/heads/tmp\nmark :{mark}\n"));
        stream.push_str(&format!(
            "committer T <t@t> {} +0000\ndata {}\n{message}",
            dates[i],
            message.len()
        ));
        if let Some(first) = ps.first() {
            stream.push_str(&format!("from :{first}\n"));
        }
        for extra in ps.iter().skip(1) {
            stream.push_str(&format!("merge :{extra}\n"));
        }
        stream.push('\n');
    }
    for (name, mark) in refs {
        stream.push_str(&format!("reset refs/heads/{name}\nfrom :{mark}\n\n"));
    }
    let mut child = std::process::Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(&repo)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("spawn fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "fast-import: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    run_git(&repo, &["update-ref", "-d", "refs/heads/tmp"]);
    crate::test_support::trust_repo(&repo);
    (dir, repo)
}

/// The stack tree as the code before S4-03 built it: one bounded
/// `rev-list --first-parent` per parentless branch, each stopping at any
/// other branch tip.
fn per_branch_oracle(
    repo: &std::path::Path,
    window: usize,
    per_branch_cap: usize,
) -> std::collections::BTreeMap<String, Option<String>> {
    let path = repo.to_str().unwrap();
    let branches = GitReader::list_branches(path).unwrap();
    let locals: Vec<&BranchInfo> = branches.iter().filter(|b| !b.is_remote).collect();
    let default_branch = locals
        .iter()
        .find(|b| b.is_default)
        .or_else(|| locals.iter().find(|b| b.is_current))
        .map(|b| b.name.clone())
        .unwrap_or_else(|| "main".to_string());
    let mut branch_tips = HashMap::new();
    for b in &locals {
        branch_tips.insert(b.name.clone(), b.tip_commit_id.clone());
    }
    let mut commit_parents = HashMap::new();
    for c in GitReader::read_commit_history(path, window, None).unwrap() {
        commit_parents.insert(c.id, c.parent_ids);
    }
    let mut nodes =
        StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, &default_branch);
    let tip_oids: HashSet<String> = branch_tips.values().cloned().collect();
    let mut enriched = false;
    for node in &nodes {
        if node.parent_branch_name.is_some() || node.branch_name == default_branch {
            continue;
        }
        let mut stop_at = tip_oids.clone();
        stop_at.remove(&node.tip_commit_id);
        let chain = if stop_at.contains(&node.tip_commit_id) {
            Some(vec![node.tip_commit_id.clone()])
        } else {
            let text = crate::engine::git_cli::git_text(
                repo,
                &[
                    "rev-list",
                    "--first-parent",
                    &format!("--max-count={per_branch_cap}"),
                    &node.tip_commit_id,
                ],
            )
            .unwrap();
            let mut walked = Vec::new();
            let mut found = None;
            for line in text.lines() {
                walked.push(line.trim().to_string());
                if stop_at.contains(line.trim()) {
                    walked.reverse();
                    found = Some(walked.clone());
                    break;
                }
            }
            found
        };
        if let Some(chain) = chain {
            for edge in chain.windows(2) {
                commit_parents
                    .entry(edge[1].clone())
                    .or_insert_with(|| vec![edge[0].clone()]);
            }
            enriched = true;
        }
    }
    if enriched {
        nodes =
            StackTreeEngine::build_stack_hierarchy(&branch_tips, &commit_parents, &default_branch);
    }
    nodes
        .into_iter()
        .map(|n| (n.branch_name, n.parent_branch_name))
        .collect()
}

fn tree_of(
    payload: &crate::stack::StackHierarchyPayload,
) -> std::collections::BTreeMap<String, Option<String>> {
    payload
        .nodes
        .iter()
        .map(|n| (n.branch_name.clone(), n.parent_branch_name.clone()))
        .collect()
}

/// xorshift: the generator must be reproducible from the seed alone.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Random DAGs (forks, merges, octopus merges, criss-crosses, twin branches,
/// skewed committer dates, several roots) against window sizes from 1 to well
/// past the history: the shared pass must give the tree the per-branch walks
/// gave.
#[test]
fn the_shared_pass_builds_the_tree_the_per_branch_walks_built() {
    let _held = crate::engine::ref_cache::retention();
    let mut compared = 0;
    let mut repaired = 0;
    for seed in 1..=30u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
        let n = 12 + rng.below(40);
        let mut parents: Vec<Vec<usize>> = Vec::new();
        let mut dates = Vec::new();
        for i in 1..=n {
            let mut ps = Vec::new();
            if i > 1 && rng.below(25) != 0 {
                ps.push(if rng.below(10) < 7 {
                    i - 1
                } else {
                    1 + rng.below(i - 1)
                });
                while rng.below(100) < 18 && ps.len() < 3 {
                    let extra = 1 + rng.below(i - 1);
                    if !ps.contains(&extra) {
                        ps.push(extra);
                    }
                }
            }
            parents.push(ps);
            let skew = if rng.below(6) == 0 {
                rng.below(60) as i64 - 30
            } else {
                0
            };
            dates.push(1_700_000_000 + i as i64 * 10 + skew);
        }
        let mut refs = vec![("main".to_string(), n)];
        for b in 0..(2 + rng.below(8)) {
            refs.push((format!("b{b}"), 1 + rng.below(n)));
        }
        let (_dir, repo) = dag_repo(&parents, &dates, &refs);
        let path = repo.to_str().unwrap();
        for window in [1, 3, 8, 21, 1000] {
            let want = per_branch_oracle(&repo, window, GitReader::MAX_HISTORY_COMMITS);
            let got = stack_hierarchy(path, window, StackWalkBudget::PRODUCTION).unwrap();
            assert_eq!(
                tree_of(&got),
                want,
                "seed {seed} window {window} parents {parents:?} refs {refs:?}"
            );
            assert_eq!(got.unresolved_branches, 0, "seed {seed} window {window}");
            compared += 1;
            if want.values().any(Option::is_some) {
                repaired += 1;
            }
        }
    }
    assert_eq!(compared, 30 * 5);
    assert!(
        repaired > 60,
        "the corpus barely places any branch: {repaired}"
    );
}

/// The old walks each had their own commit cap; the shared pass has one. A
/// deep unrelated branch whose commits are newer than a shallow branch's fork
/// point must not spend the budget that branch needed: `a` sits on `x` two
/// commits above it and the old walk found that in three commits, whatever
/// `hog` was doing.
#[test]
fn a_deep_branch_does_not_spend_the_commits_a_shallow_branch_needed() {
    let _held = crate::engine::ref_cache::retention();
    let mut parents: Vec<Vec<usize>> = Vec::new();
    let mut dates: Vec<i64> = Vec::new();
    for i in 1..=30usize {
        parents.push(if i == 1 { vec![] } else { vec![i - 1] });
        dates.push(1_700_000_000 + i as i64 * 10);
    }
    // a: 31, 32 on top of main's commit 5 (where x is); older than the hog.
    parents.push(vec![5]);
    dates.push(1_700_000_000 + 51);
    parents.push(vec![31]);
    dates.push(1_700_000_000 + 52);
    // hog: 80 commits sharing no history, all newer than anything above.
    for k in 0..80usize {
        parents.push(if k == 0 { vec![] } else { vec![33 + k - 1] });
        dates.push(1_700_100_000 + k as i64);
    }
    let refs = vec![
        ("main".to_string(), 30),
        ("x".to_string(), 5),
        ("a".to_string(), 32),
        ("hog".to_string(), 112),
    ];
    let (_dir, repo) = dag_repo(&parents, &dates, &refs);
    let cap = 50;
    let want = per_branch_oracle(&repo, 3, cap);
    assert_eq!(want["a"], Some("x".to_string()), "the oracle places a on x");
    let budget = StackWalkBudget {
        max_commits: cap,
        ..StackWalkBudget::PRODUCTION
    };
    let got = stack_hierarchy(repo.to_str().unwrap(), 3, budget).unwrap();
    assert_eq!(tree_of(&got), want);
    // The hog itself is the branch nothing could settle, and it is counted.
    assert!(got.unresolved_branches >= 1);
    assert!(got.walk_incomplete);
}

/// The kept answer of the pass must follow every way the branch set can
/// change between ticks: a branch force-moved to an OLDER commit, a deleted
/// branch, a new branch on an existing tip, and a pack-refs that rewrites the
/// ref store without moving a ref. After each step the tree is the one the
/// per-branch walks give on the repository as it now is.
#[test]
fn the_kept_pass_follows_force_moves_deletes_twins_and_packed_refs() {
    let _held = crate::engine::ref_cache::retention();
    let (_dir, repo) = stack_chain(150, &twenty_deep_branches(), false);
    let path = repo.to_str().unwrap();
    crate::engine::ref_cache::eager(|| {
        let check = |step: &str| {
            let got = stack_hierarchy(path, FIXTURE_WINDOW, StackWalkBudget::PRODUCTION)
                .unwrap_or_else(|e| panic!("{step}: {e}"));
            let want = per_branch_oracle(&repo, FIXTURE_WINDOW, GitReader::MAX_HISTORY_COMMITS);
            assert_eq!(tree_of(&got), want, "{step}");
        };
        check("cold");
        check("idle tick");
        // b10 force-moved to an older commit than b05's tip.
        let older = crate::engine::git_cli::git_text(&repo, &["rev-parse", "main~145"]).unwrap();
        run_git(&repo, &["update-ref", "refs/heads/b10", older.trim()]);
        check("force-moved to an older commit");
        // b03 deleted: b04 now sits on b02.
        run_git(&repo, &["branch", "-D", "b03"]);
        check("deleted branch");
        // A new branch on b07's tip.
        run_git(&repo, &["branch", "twin", "b07"]);
        check("new branch on an existing tip");
        run_git(&repo, &["pack-refs", "--all"]);
        check("packed refs only");
        // A ref that moves back to where it was: the answer kept for that
        // exact argv must still be the right one.
        let b10 = crate::engine::git_cli::git_text(&repo, &["rev-parse", "main~97"]).unwrap();
        run_git(&repo, &["update-ref", "refs/heads/b10", b10.trim()]);
        check("moved back");
    });
}

/// Why the pass has no `--not` stop set. Excluding the other branch tips (the
/// obvious set) also excludes everything a tip sitting MID-chain can reach:
/// `y` forks off `a`'s own chain at commit 5, so `--not y` removes 5, 4 and 3
/// from `a`'s walk and the base `x` (commit 3) is never seen, although `a`
/// sits on it. The pass without `--not` places `a`.
#[test]
fn excluding_other_tips_would_cut_a_chain_a_mid_chain_tip_shares() {
    let _held = crate::engine::ref_cache::retention();
    // 1 - 2 - 3(x) - 4 - 5 - 6(a); y is commit 7 on top of 5.
    let parents = vec![vec![], vec![1], vec![2], vec![3], vec![4], vec![5], vec![5]];
    let dates: Vec<i64> = (1..=7).map(|i| 1_700_000_000 + i * 10).collect();
    let refs = vec![
        ("main".to_string(), 1),
        ("x".to_string(), 3),
        ("a".to_string(), 6),
        ("y".to_string(), 7),
    ];
    let (_dir, repo) = dag_repo(&parents, &dates, &refs);
    let oid = |name: &str| {
        crate::engine::git_cli::git_text(&repo, &["rev-parse", name])
            .unwrap()
            .trim()
            .to_string()
    };
    let with_not = crate::engine::git_cli::git_text(
        &repo,
        &[
            "rev-list",
            "--first-parent",
            &oid("a"),
            "--not",
            &oid("x"),
            &oid("y"),
            &oid("main"),
        ],
    )
    .unwrap();
    assert_eq!(
        with_not.lines().collect::<Vec<_>>(),
        vec![oid("a").as_str()],
        "with the other tips excluded the walk stops at a's own tip"
    );
    let payload = stack_hierarchy(repo.to_str().unwrap(), 1, StackWalkBudget::PRODUCTION).unwrap();
    assert_eq!(parent_of(&payload, "a"), Some("x".to_string()));
    assert_eq!(payload.unresolved_branches, 0);
}
