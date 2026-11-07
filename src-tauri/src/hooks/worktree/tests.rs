use super::*;
use crate::engine::worktree::agent_layout;
use crate::harness::PolicyStatus;
use crate::test_support::git_in;
use std::fs;
use std::sync::atomic::AtomicI32;
use std::sync::Mutex;

/// A verdict as the gate would return it.
fn verdict(status: PolicyStatus, detail_code: &str) -> PolicyVerdict {
    PolicyVerdict {
        status,
        checked: status != PolicyStatus::Unchecked,
        target: String::new(),
        rule: if status == PolicyStatus::Blocked {
            "command.test_rule".into()
        } else {
            String::new()
        },
        severity: String::new(),
        reason: "the test said so".into(),
        demoted: String::new(),
        grant_id: String::new(),
        granted_by: String::new(),
        widened: String::new(),
        degraded: Vec::new(),
        task_id: String::new(),
        detail: String::new(),
        detail_code: detail_code.into(),
    }
}

/// A gate that answers `status` and records every argv it was asked about,
/// so a test can say which git writes were judged.
struct Recorder {
    status: PolicyStatus,
    detail_code: &'static str,
    seen: Mutex<Vec<Vec<String>>>,
}

impl Recorder {
    fn answering(status: PolicyStatus) -> Self {
        Self {
            status,
            detail_code: "",
            seen: Mutex::new(Vec::new()),
        }
    }

    fn judge(&self, _repo: &str, argv: &[&str]) -> Result<PolicyVerdict, String> {
        self.seen
            .lock()
            .unwrap()
            .push(argv.iter().map(|a| a.to_string()).collect());
        Ok(verdict(self.status, self.detail_code))
    }

    fn seen(&self) -> Vec<Vec<String>> {
        self.seen.lock().unwrap().clone()
    }
}

fn input_for_create(cwd: &Path, name: &str) -> HookInput {
    HookInput {
        hook_event_name: "WorktreeCreate".into(),
        cwd: cwd.to_string_lossy().into_owned(),
        worktree_name: name.into(),
        ..HookInput::default()
    }
}

fn input_for_remove(path: &Path, discard: bool) -> HookInput {
    HookInput {
        hook_event_name: "WorktreeRemove".into(),
        worktree_path: path.to_string_lossy().into_owned(),
        discard_changes: discard,
        ..HookInput::default()
    }
}

fn head(dir: &Path, rev: &str) -> String {
    crate::engine::git_cli::git_text(dir, &["rev-parse", rev])
        .unwrap()
        .trim()
        .to_string()
}

fn commit(dir: &Path, file: &str, text: &str, message: &str) {
    fs::write(dir.join(file), text).unwrap();
    git_in(dir, &["add", file]);
    git_in(dir, &["commit", "-q", "-m", message]);
}

/// A trusted repository with an `origin` whose `main` is one commit behind the
/// local `main`, and a checked-out feature branch one commit past local
/// `main`: three different answers to "what does a new worktree start from".
struct Fixture {
    _dirs: Vec<tempfile::TempDir>,
    root: PathBuf,
    origin_main: String,
    local_main: String,
    feature: String,
}

fn fixture_with(setup: impl FnOnce(&Path)) -> Fixture {
    let origin = tempfile::tempdir().unwrap();
    git_in(origin.path(), &["init", "-q", "--bare", "-b", "main"]);
    let main = tempfile::tempdir().unwrap();
    let root = main.path().canonicalize().unwrap();
    git_in(&root, &["init", "-q", "-b", "main"]);
    fs::write(root.join(".gitignore"), ".env\nnode_modules/\n").unwrap();
    setup(&root);
    git_in(&root, &["add", "."]);
    git_in(&root, &["commit", "-q", "-m", "initial"]);
    git_in(
        &root,
        &["remote", "add", "origin", origin.path().to_str().unwrap()],
    );
    git_in(&root, &["push", "-q", "-u", "origin", "main"]);
    git_in(&root, &["remote", "set-head", "origin", "main"]);
    let origin_main = head(&root, "HEAD");
    commit(&root, "local.txt", "unpushed\n", "local only");
    let local_main = head(&root, "HEAD");
    git_in(&root, &["checkout", "-q", "-b", "feature"]);
    commit(&root, "feature.txt", "feature\n", "feature work");
    let feature = head(&root, "HEAD");
    Fixture {
        _dirs: vec![origin, main],
        root,
        origin_main,
        local_main,
        feature,
    }
}

fn trusted() -> Fixture {
    let fixture = fixture_with(|_| {});
    crate::test_support::trust_repo(&fixture.root);
    fixture
}

fn progress() -> AtomicI32 {
    AtomicI32::new(EXIT_UNAVAILABLE)
}

fn created(fixture: &Fixture, name: &str) -> PathBuf {
    created_from(&fixture.root, name)
}

/// A worktree made by a session whose `cwd` is `cwd`.
fn created_from(cwd: &Path, name: &str) -> PathBuf {
    let gate = Recorder::answering(PolicyStatus::Allowed);
    let finished = create(
        &input_for_create(cwd, name),
        &|r, a| gate.judge(r, a),
        &progress(),
    );
    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    PathBuf::from(finished.path.expect("a path"))
}

/* ── create ──────────────────────────────────────────────────────────────── */

#[test]
fn creates_at_claude_codes_path_on_its_branch_from_the_local_default() {
    let fixture = trusted();
    let gate = Recorder::answering(PolicyStatus::Allowed);
    let marker = progress();

    let finished = create(
        &input_for_create(&fixture.root, "bold-oak-a3f2"),
        &|r, a| gate.judge(r, a),
        &marker,
    );

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    let expected = fixture.root.join(".claude/worktrees/bold-oak-a3f2");
    assert_eq!(finished.path.as_deref(), expected.to_str());
    // Claude Code's attribution keeps working on the path it would have used.
    let layout = agent_layout(expected.to_str().unwrap()).expect("an agent layout");
    assert_eq!(
        (layout.kind.as_str(), layout.slug.as_str()),
        ("claude", "bold-oak-a3f2")
    );
    // Its branch, cut from local main: not origin/main (behind) and not the
    // feature branch the session happens to have checked out.
    assert_eq!(
        crate::engine::git_cli::git_text(&expected, &["symbolic-ref", "--short", "HEAD"])
            .unwrap()
            .trim(),
        "worktree-bold-oak-a3f2"
    );
    let base = head(&expected, "HEAD");
    assert_eq!(base, fixture.local_main);
    assert_ne!(base, fixture.origin_main);
    assert_ne!(base, fixture.feature);
    // The one git write was judged, as the exact command the engine runs.
    let target = expected.to_str().unwrap();
    assert_eq!(
        gate.seen(),
        vec![add_worktree_argv(
            target,
            Some("worktree-bold-oak-a3f2"),
            Some("main"),
            false
        )]
    );
    assert_eq!(
        marker.load(Ordering::SeqCst),
        EXIT_FAILED,
        "the write was marked as started"
    );
}

#[test]
fn the_base_falls_back_to_head_when_the_default_branch_is_not_local() {
    let fixture = trusted();
    // origin/HEAD still names `main`, which no longer exists locally.
    git_in(&fixture.root, &["branch", "-q", "-m", "main", "renamed"]);
    assert_eq!(local_default_base(&fixture.root), None);
    let path = created(&fixture, "no-default");
    assert_eq!(head(&path, "HEAD"), fixture.feature);
}

#[test]
fn an_untrusted_repository_is_handed_back_to_plain_git_untouched() {
    // The owner's decision: plain git, not a refusal. GitPulse spawns nothing
    // in a repository it was not asked to trust, so it does nothing at all and
    // says so with the exit code the launcher falls back on.
    let fixture = fixture_with(|_| {});
    // `git_in(["init"])` trusts every fixture it makes; this one must not be.
    crate::repository_trust::revoke(fixture.root.to_str().unwrap()).expect("revoked");
    assert!(crate::repository_trust::require(&fixture.root).is_err());
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "plain"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_UNAVAILABLE, "{finished:?}");
    assert_eq!(finished.path, None);
    assert!(
        finished.notes.iter().any(|n| n.contains("is not trusted")),
        "{finished:?}"
    );
    assert!(!fixture.root.join(".claude").exists());
    assert!(
        gate.seen().is_empty(),
        "nothing was judged, because nothing was run"
    );

    // Removal takes the same road back.
    let somewhere = fixture.root.join("sub");
    fs::create_dir(&somewhere).unwrap();
    git_in(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            somewhere.to_str().unwrap(),
        ],
    );
    let finished = remove(
        &input_for_remove(&somewhere, false),
        &|r, a| gate.judge(r, a),
        &progress(),
    );
    assert_eq!(finished.exit_code, EXIT_UNAVAILABLE, "{finished:?}");
    assert!(somewhere.join(".git").is_file());
}

#[test]
fn a_policy_refusal_creates_nothing_and_does_not_fall_back() {
    let fixture = trusted();
    let gate = Recorder::answering(PolicyStatus::Blocked);

    let finished = create(
        &input_for_create(&fixture.root, "refused"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert_eq!(finished.path, None);
    assert!(finished
        .notes
        .iter()
        .any(|n| n.contains("refused by policy")));
    assert!(!fixture.root.join(".claude/worktrees/refused").exists());
}

#[test]
fn a_gate_that_could_not_run_is_a_gitpulse_failure_plain_git_recovers_from() {
    let fixture = trusted();
    let gate = Recorder {
        status: PolicyStatus::Unchecked,
        detail_code: "timeout",
        seen: Mutex::new(Vec::new()),
    };
    let marker = progress();

    let finished = create(
        &input_for_create(&fixture.root, "gate-down"),
        &|r, a| gate.judge(r, a),
        &marker,
    );

    assert_eq!(finished.exit_code, EXIT_UNAVAILABLE, "{finished:?}");
    assert!(
        finished
            .notes
            .iter()
            .any(|n| n.starts_with("GitPulse failure")),
        "{finished:?}"
    );
    assert!(!fixture.root.join(".claude/worktrees/gate-down").exists());
    assert_eq!(marker.load(Ordering::SeqCst), EXIT_UNAVAILABLE);
}

#[test]
fn a_git_failure_is_told_apart_from_a_gitpulse_failure() {
    let fixture = trusted();
    // The branch Claude Code's name maps to already exists: git refuses.
    git_in(&fixture.root, &["branch", "worktree-taken"]);
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "taken"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert_eq!(finished.path, None);
    assert!(
        finished.notes.iter().any(|n| n.starts_with("git failed")),
        "{finished:?}"
    );
}

#[test]
fn names_that_could_escape_or_break_the_branch_are_refused() {
    let fixture = trusted();
    let gate = Recorder::answering(PolicyStatus::Allowed);
    for name in [
        "", "../x", "a/b", ".hidden", "-flag", "a..b", "x.lock", "dot.", "sp ace", "ü",
    ] {
        let finished = create(
            &input_for_create(&fixture.root, name),
            &|r, a| gate.judge(r, a),
            &progress(),
        );
        assert_eq!(finished.exit_code, EXIT_FAILED, "{name:?}: {finished:?}");
        assert_eq!(finished.path, None);
    }
    assert!(gate.seen().is_empty());
}

#[cfg(unix)]
#[test]
fn a_container_that_is_a_link_is_refused() {
    let fixture = trusted();
    let elsewhere = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(elsewhere.path(), fixture.root.join(".claude")).unwrap();
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "through-a-link"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert!(fs::read_dir(elsewhere.path()).unwrap().next().is_none());
}

#[test]
fn caches_and_the_include_are_copied_as_for_an_app_created_worktree() {
    let fixture = trusted();
    fs::create_dir_all(fixture.root.join("node_modules/pkg")).unwrap();
    fs::write(fixture.root.join("node_modules/pkg/index.js"), "x").unwrap();
    fs::write(fixture.root.join(".worktreeinclude"), ".env\n").unwrap();
    fs::write(fixture.root.join(".env"), "SECRET=1").unwrap();

    let path = created(&fixture, "with-caches");

    assert!(path.join("node_modules/pkg/index.js").is_file());
    assert_eq!(fs::read_to_string(path.join(".env")).unwrap(), "SECRET=1");
}

const HOOKS: &str =
    r#"{"worktree":{"post_create":["printf '%s' \"$GITPULSE_WORKTREE_PATH\" > hooked.txt"]}}"#;

fn with_hooks(hooks: &'static str) -> Fixture {
    let fixture = fixture_with(|root| {
        fs::create_dir_all(root.join(".gitpulse")).unwrap();
        fs::write(root.join(".gitpulse/hooks.json"), hooks).unwrap();
        // The hook's own output is not part of the checkout.
        fs::write(root.join(".gitignore"), ".env\nnode_modules/\nhooked.txt\n").unwrap();
    });
    crate::test_support::trust_repo(&fixture.root);
    fixture
}

#[cfg(unix)]
#[test]
fn approved_post_create_hooks_run_with_the_gitpulse_environment() {
    let fixture = with_hooks(HOOKS);
    crate::engine::approve_worktree_hooks(&fixture.root, HOOKS).expect("approved");

    let path = created(&fixture, "hooked");

    assert_eq!(
        fs::read_to_string(path.join("hooked.txt")).unwrap(),
        path.to_str().unwrap()
    );
}

#[cfg(unix)]
#[test]
fn an_unapproved_hooks_file_runs_nothing_and_says_so() {
    let fixture = with_hooks(HOOKS);
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "unapproved"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    let path = PathBuf::from(finished.path.clone().unwrap());
    assert!(!path.join("hooked.txt").exists());
    assert!(
        finished
            .notes
            .iter()
            .any(|n| n.contains("post_create hooks did not run")),
        "{finished:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_post_create_hook_keeps_the_worktree_and_says_so() {
    // As the Worktrees panel does: the worktree exists and is handed over, and
    // the session is told its setup did not finish.
    const FAILING: &str = r#"{"worktree":{"post_create":["echo setup broke >&2; exit 3"]}}"#;
    let fixture = with_hooks(FAILING);
    crate::engine::approve_worktree_hooks(&fixture.root, FAILING).expect("approved");
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "broken-setup"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    let path = fixture.root.join(".claude/worktrees/broken-setup");
    assert_eq!(finished.path.as_deref(), path.to_str());
    assert!(path.join(".git").is_file());
    assert!(
        finished
            .notes
            .iter()
            .any(|n| n.contains("post_create hook failed") && n.contains("kept")),
        "{finished:?}"
    );
    assert!(crate::engine::git_cli::git_text(
        &fixture.root,
        &["rev-parse", "--verify", "refs/heads/worktree-broken-setup"]
    )
    .is_ok());
    // Only the add was written: nothing was undone.
    assert_eq!(gate.seen().len(), 1, "{:?}", gate.seen());

    // Claude Code shows the session none of the hook's stderr on success, so
    // the session's own brief is what tells it, read from the worktree's git
    // directory, never from its files.
    let warning = setup_failure(&path).expect("the failure is recorded for the session");
    assert!(warning.contains("setup did not finish"), "{warning}");
    assert!(warning.contains("exit"), "{warning}");
    let status = crate::engine::git_cli::git_text(&path, &["status", "--porcelain"]).unwrap();
    assert_eq!(
        status.trim(),
        "",
        "the marker is not in the working tree: {status}"
    );
    let brief = super::super::run_session_brief(&HookInput {
        hook_event_name: "SessionStart".into(),
        cwd: path.to_string_lossy().into_owned(),
        source: "startup".into(),
        ..HookInput::default()
    });
    let context = brief
        .hook_specific_output
        .and_then(|o| o.additional_context)
        .expect("a brief");
    assert!(
        context.starts_with("WARNING: this worktree's setup did not finish"),
        "{context}"
    );

    // And a worktree whose setup finished says nothing of the kind.
    assert_eq!(setup_failure(&fixture.root), None);
}

#[test]
fn a_second_run_for_the_same_name_hands_over_the_first_ones_worktree() {
    // The plugin's registration and a settings copy both run for one worktree.
    let fixture = trusted();
    let gate = Recorder::answering(PolicyStatus::Allowed);
    let input = input_for_create(&fixture.root, "twice");

    let first = create(&input, &|r, a| gate.judge(r, a), &progress());
    let second = create(&input, &|r, a| gate.judge(r, a), &progress());

    assert_eq!(first.exit_code, EXIT_DONE, "{first:?}");
    assert_eq!(second.exit_code, EXIT_DONE, "{second:?}");
    assert_eq!(first.path, second.path);
    assert!(
        second.notes.iter().any(|n| n.contains("already made")),
        "{second:?}"
    );
    assert_eq!(gate.seen().len(), 1, "one add, judged once");
}

#[test]
fn two_runs_at_once_make_one_worktree_and_both_hand_it_over() {
    let fixture = trusted();
    let gate = Recorder::answering(PolicyStatus::Allowed);
    let input = input_for_create(&fixture.root, "racing");

    let (a, b) = std::thread::scope(|scope| {
        let a = scope.spawn(|| create(&input, &|r, v| gate.judge(r, v), &progress()));
        let b = scope.spawn(|| create(&input, &|r, v| gate.judge(r, v), &progress()));
        (a.join().unwrap(), b.join().unwrap())
    });

    assert_eq!(
        (a.exit_code, b.exit_code),
        (EXIT_DONE, EXIT_DONE),
        "{a:?} {b:?}"
    );
    assert_eq!(a.path, b.path);
    assert_eq!(gate.seen().len(), 1, "exactly one git worktree add");
}

#[test]
fn something_else_at_the_path_is_never_handed_over() {
    let fixture = trusted();
    let path = created(&fixture, "moved-on");
    // The worktree now holds another branch: not what this hook made.
    git_in(&path, &["checkout", "-q", "-b", "elsewhere"]);
    let gate = Recorder::answering(PolicyStatus::Allowed);

    let finished = create(
        &input_for_create(&fixture.root, "moved-on"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );

    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert_eq!(finished.path, None);
    assert!(finished.notes.iter().any(|n| n.contains("already exists")));
    // A plain directory there is refused the same way.
    fs::create_dir_all(fixture.root.join(".claude/worktrees/squatter")).unwrap();
    let finished = create(
        &input_for_create(&fixture.root, "squatter"),
        &|r, a| gate.judge(r, a),
        &progress(),
    );
    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert!(gate.seen().is_empty());
}

#[test]
fn only_a_worktree_git_registered_counts_as_ready() {
    let fixture = trusted();
    let (_, common) = crate::repository_trust::git_directories(&fixture.root).unwrap();
    let path = created(&fixture, "registered");
    assert!(is_ready(&path, &common));

    // A hand-written gitfile pointing at the real registry entry is not this
    // worktree: the entry names its own checkout back.
    let impostor = fixture.root.join(".claude/worktrees/impostor");
    fs::create_dir_all(&impostor).unwrap();
    fs::copy(path.join(".git"), impostor.join(".git")).unwrap();
    assert!(!is_ready(&impostor, &common));
    // Nor is a directory with no gitfile, or one naming another repository.
    let bare = fixture.root.join(".claude/worktrees/bare");
    fs::create_dir_all(&bare).unwrap();
    assert!(!is_ready(&bare, &common));
    let other = trusted();
    let (_, other_common) = crate::repository_trust::git_directories(&other.root).unwrap();
    assert!(!is_ready(&path, &other_common));
}

/* ── remove ──────────────────────────────────────────────────────────────── */

fn removing(path: &Path, discard: bool) -> (Finished, Recorder) {
    let gate = Recorder::answering(PolicyStatus::Allowed);
    let finished = remove(
        &input_for_remove(path, discard),
        &|r, a| gate.judge(r, a),
        &progress(),
    );
    (finished, gate)
}

#[test]
fn a_clean_worktree_is_removed_through_the_app_path_with_its_branch() {
    let fixture = trusted();
    let path = created(&fixture, "clean");
    // Ignored files are not work: a copied .env does not hold the removal up.
    fs::write(path.join(".env"), "SECRET=1").unwrap();

    let (finished, gate) = removing(&path, false);

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    assert!(!path.exists());
    assert!(
        finished
            .notes
            .iter()
            .any(|n| n == "deleted branch worktree-clean"),
        "{finished:?}"
    );
    let seen = gate.seen();
    assert_eq!(seen[0], remove_worktree_argv(path.to_str().unwrap(), false));
    assert_eq!(seen[1], ["git", "branch", "-d", "worktree-clean"]);
}

#[test]
fn work_in_a_removed_worktree_is_discarded_as_claude_code_confirmed_and_said() {
    // Claude Code asks before removing a worktree with work in it and never
    // sends `discard_changes` to the hook, so a removal that arrives is a
    // confirmed one: forced, with `-D`, and the discarded work is named.
    type MakeWork<'a> = &'a dyn Fn(&Path);
    let fixture = trusted();
    let cases: [(&str, MakeWork<'_>); 3] = [
        ("dirty", &|p: &Path| {
            fs::write(p.join("local.txt"), "edited\n").unwrap()
        }),
        ("untracked", &|p: &Path| {
            fs::write(p.join("new.txt"), "new\n").unwrap()
        }),
        ("unpushed", &|p: &Path| {
            commit(p, "work.txt", "work\n", "only here")
        }),
    ];
    for (name, make_work) in cases {
        let path = created(&fixture, name);
        make_work(&path);

        let (finished, gate) = removing(&path, false);

        assert_eq!(finished.exit_code, EXIT_DONE, "{name}: {finished:?}");
        assert!(!path.exists(), "{name}: the worktree is still there");
        let seen = gate.seen();
        assert_eq!(
            seen[0],
            remove_worktree_argv(path.to_str().unwrap(), true),
            "{name}"
        );
        assert_eq!(
            seen[1],
            ["git", "branch", "-D", &format!("worktree-{name}")],
            "{name}"
        );
        assert!(
            finished.notes.iter().any(|n| n.contains("discarded")),
            "{name}: {finished:?}"
        );
    }
}

#[test]
fn a_work_check_that_cannot_run_is_never_read_as_nothing_to_lose() {
    // Not a repository: status cannot be read, so there is no answer.
    let elsewhere = tempfile::tempdir().unwrap();
    assert!(work_held(elsewhere.path(), None).is_err());
}

#[test]
fn a_commit_another_branch_holds_is_not_work_lost() {
    let fixture = trusted();
    let path = created(&fixture, "merged-elsewhere");
    commit(&path, "work.txt", "work\n", "kept by a branch");
    git_in(
        &fixture.root,
        &["branch", "keeper", "worktree-merged-elsewhere"],
    );

    let (finished, _) = removing(&path, false);

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    assert!(!path.exists());
}

#[test]
fn discard_changes_removes_the_work_and_the_branch() {
    let fixture = trusted();
    let path = created(&fixture, "discarded");
    commit(&path, "work.txt", "work\n", "only here");
    fs::write(path.join("local.txt"), "edited\n").unwrap();

    let (finished, gate) = removing(&path, true);

    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    assert!(!path.exists());
    let seen = gate.seen();
    assert_eq!(seen[0], remove_worktree_argv(path.to_str().unwrap(), true));
    assert_eq!(seen[1], ["git", "branch", "-D", "worktree-discarded"]);
}

#[test]
fn a_session_inside_a_linked_worktree_gets_its_worktree_under_the_main_checkout() {
    // Where Claude Code 2.1.319 puts it with no hook at all (run live): under
    // the main checkout, never nested in the linked worktree the session was
    // started in, here a GitPulse task lane. The hook anchors there whatever
    // `cwd` the payload carries.
    let fixture = trusted();
    let lane = fixture.root.join(".gitpulse/worktrees/lane-1");
    git_in(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "gitpulse/lane-1",
            lane.to_str().unwrap(),
            "main",
        ],
    );
    let lane = lane.canonicalize().unwrap();

    let path = created_from(&lane, "inner");
    assert_eq!(path, fixture.root.join(".claude/worktrees/inner"));
    assert!(!lane.join(".claude").exists());
    let layout = agent_layout(path.to_str().unwrap()).expect("an agent layout");
    assert_eq!(
        (layout.kind.as_str(), layout.slug.as_str()),
        ("claude", "inner")
    );

    let (finished, gate) = removing(&path, false);
    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
    assert_eq!(gate.seen()[1], ["git", "branch", "-d", "worktree-inner"]);
}

#[test]
fn the_branch_to_delete_is_named_by_the_innermost_claude_worktree() {
    let nested = Path::new("/r/.claude/worktrees/a/.claude/worktrees/b");
    assert!(made_by_claude(nested, "worktree-b"));
    assert!(!made_by_claude(nested, "worktree-a"));
    assert!(made_by_claude(
        Path::new("/r/.gitpulse/worktrees/l/.claude/worktrees/x"),
        "worktree-x"
    ));
    assert!(!made_by_claude(Path::new("/r/.claude/worktrees/x"), "main"));
    assert!(!made_by_claude(Path::new("/r/elsewhere/x"), "worktree-x"));
}

#[test]
fn the_main_checkout_and_a_refused_removal_are_kept() {
    let fixture = trusted();
    let (finished, gate) = removing(&fixture.root, true);
    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert!(gate.seen().is_empty());

    let path = created(&fixture, "policy-kept");
    let gate = Recorder::answering(PolicyStatus::Blocked);
    let finished = remove(
        &input_for_remove(&path, false),
        &|r, a| gate.judge(r, a),
        &progress(),
    );
    assert_eq!(finished.exit_code, EXIT_FAILED, "{finished:?}");
    assert!(path.join(".git").is_file());
}

#[test]
fn a_worktree_already_gone_counts_as_removed() {
    let gone = tempfile::tempdir().unwrap().path().join("never-there");
    let (finished, _) = removing(&gone, false);
    assert_eq!(finished.exit_code, EXIT_DONE, "{finished:?}");
}

/* ── the deadline, the manifest and the launcher ─────────────────────────── */

#[test]
fn running_out_of_time_prints_no_path_and_says_whether_git_had_started() {
    static BEFORE: AtomicI32 = AtomicI32::new(0);
    let finished = run_within(Duration::from_millis(50), &BEFORE, |_| {
        std::thread::sleep(Duration::from_secs(2));
        Finished::done(Some("/never".into()), Vec::new())
    });
    assert_eq!(
        finished.exit_code, EXIT_UNAVAILABLE,
        "nothing written: plain git may"
    );
    assert_eq!(finished.path, None);

    static AFTER: AtomicI32 = AtomicI32::new(0);
    let finished = run_within(Duration::from_millis(50), &AFTER, |progress| {
        mark_write_started(progress);
        std::thread::sleep(Duration::from_secs(2));
        Finished::done(Some("/never".into()), Vec::new())
    });
    assert_eq!(
        finished.exit_code, EXIT_FAILED,
        "a write started: no fallback over it"
    );
    assert_eq!(finished.path, None);
}

fn manifest() -> serde_json::Value {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../plugins/gitpulse/hooks/hooks.json");
    serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn the_plugin_registers_both_events_through_the_launcher_inside_the_deadline() {
    let manifest = manifest();
    for (event, subcommand) in [("WorktreeCreate", CREATE), ("WorktreeRemove", REMOVE)] {
        let handlers = manifest["hooks"][event][0]["hooks"]
            .as_array()
            .unwrap_or_else(|| panic!("{event} is not registered"));
        assert_eq!(handlers.len(), 1, "{event}");
        let handler = &handlers[0];
        assert_eq!(
            handler["command"].as_str(),
            Some(format!("{LAUNCHER_PREFIX}gitpulse-hook {subcommand}").as_str())
        );
        assert!(handler.get("async").is_none(), "{event} must be waited for");
        let timeout = handler["timeout"].as_u64().expect("an explicit timeout");
        assert_eq!(timeout, HOST_TIMEOUT_SECS);
        // The work, then printing and reaping, end before the host kills us.
        let worst =
            WORKTREE_BUDGET + super::super::HOST_TIMEOUT_MARGIN + crate::procguard::CHILD_GRACE;
        assert!(
            worst < Duration::from_secs(timeout),
            "{event}: {worst:?} >= {timeout}s"
        );
    }
    assert!(Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/gitpulse")
        .join(LAUNCHER)
        .is_file());
}

/// Runs the shipped launcher with `binary` as the hook binary.
#[cfg(unix)]
fn launch(
    binary: &str,
    event: &str,
    payload: &serde_json::Value,
    cwd: &Path,
) -> std::process::Output {
    use crate::procguard::LockedSpawn;
    use std::io::Write;
    let launcher = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../plugins/gitpulse")
        .join(LAUNCHER);
    let mut child = std::process::Command::new("sh")
        .arg(launcher)
        .args([binary, event])
        .current_dir(cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn_locked()
        .expect("sh runs");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

#[cfg(unix)]
#[test]
fn the_launcher_makes_a_plain_worktree_when_the_binary_cannot_serve() {
    let fixture = fixture_with(|_| {});
    let payload = serde_json::json!({
        "hook_event_name": "WorktreeCreate",
        "cwd": fixture.root,
        "name": "plain-oak",
    });

    let out = launch("/no/such/gitpulse-hook", CREATE, &payload, &fixture.root);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let expected = fixture.root.join(".claude/worktrees/plain-oak");
    // The path, and only the path, on stdout.
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", expected.display())
    );
    assert!(stderr.contains("plain git will"), "{stderr}");
    assert_eq!(
        head(&expected, "HEAD"),
        fixture.local_main,
        "the same base rule as GitPulse's"
    );

    // Removal of a worktree holding work: Claude Code confirmed it, so plain
    // git forces it, and keeps the branch, so the commit stays reachable.
    commit(&expected, "work.txt", "work\n", "only here");
    let tip = head(&expected, "HEAD");
    fs::write(expected.join("new.txt"), "work").unwrap();
    let remove =
        serde_json::json!({"hook_event_name": "WorktreeRemove", "worktree_path": expected});
    let out = launch("/no/such/gitpulse-hook", REMOVE, &remove, &fixture.root);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    assert!(!expected.exists());
    assert!(stderr.contains("discarded"), "{stderr}");
    assert_eq!(
        head(&fixture.root, "worktree-plain-oak"),
        tip,
        "the branch was kept"
    );

    // A path the launcher cannot read as a worktree is kept, never guessed at.
    let not_a_worktree = fixture.root.join("plain-dir");
    fs::create_dir(&not_a_worktree).unwrap();
    let remove = serde_json::json!({"worktree_path": not_a_worktree});
    let out = launch("/no/such/gitpulse-hook", REMOVE, &remove, &fixture.root);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(not_a_worktree.is_dir());
}

#[cfg(unix)]
#[test]
fn the_launchers_plain_worktree_also_goes_under_the_main_checkout() {
    // The same placement as GitPulse's own path and Claude Code's.
    let fixture = fixture_with(|_| {});
    let lane = fixture.root.join("lane");
    git_in(
        &fixture.root,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "lane",
            lane.to_str().unwrap(),
            "main",
        ],
    );
    let lane = lane.canonicalize().unwrap();
    let payload = serde_json::json!({"cwd": lane, "name": "from-a-lane"});

    let out = launch("/no/such/gitpulse-hook", CREATE, &payload, &lane);

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(0), "{stderr}");
    let expected = fixture.root.join(".claude/worktrees/from-a-lane");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("{}\n", expected.display())
    );
    assert!(!lane.join(".claude").exists());
}

#[cfg(unix)]
#[test]
fn the_launcher_falls_back_only_on_75_and_passes_every_other_answer_through() {
    let fixture = fixture_with(|_| {});
    let dir = tempfile::tempdir().unwrap();
    let fake = dir.path().join("gitpulse-hook");
    let payload = serde_json::json!({"cwd": fixture.root, "name": "from-fake"});
    for (code, falls_back) in [(75, true), (1, false), (0, false)] {
        fs::write(
            &fake,
            format!(
                "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'gitpulse-hook 0\\nsubcommands: notify, worktree-create, worktree-remove\\n'; exit 0; fi\ncat >/dev/null\nexit {code}\n"
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();

        let out = launch(fake.to_str().unwrap(), CREATE, &payload, &fixture.root);

        let made = fixture.root.join(".claude/worktrees/from-fake");
        if falls_back {
            assert_eq!(
                out.status.code(),
                Some(0),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(made.join(".git").is_file());
            git_in(
                &fixture.root,
                &["worktree", "remove", made.to_str().unwrap()],
            );
            git_in(&fixture.root, &["branch", "-q", "-D", "worktree-from-fake"]);
        } else {
            assert_eq!(out.status.code(), Some(code));
            assert!(!made.exists(), "exit {code} fell back");
        }
    }

    // A binary too old to list the subcommand is not asked at all.
    fs::write(&fake, "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'gitpulse-hook 0\\nsubcommands: notify\\n'; exit 0; fi\nexit 0\n").unwrap();
    let out = launch(fake.to_str().unwrap(), CREATE, &payload, &fixture.root);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("older than this plugin"));
}
