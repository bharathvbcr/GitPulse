//! End-to-end coverage for the stash, remote, submodule, replay and reset
//! surfaces against real repositories driven by the real `git` binary.
//!
//! Each case exercises the property that made the surface worth building, not
//! merely that the command exits zero: that a stale stash index cannot destroy
//! the wrong entry, that a hostile remote URL never reaches argv, that an
//! uninitialized submodule is distinguishable from a broken one, and that a
//! replay refuses to start on top of a parked operation.

use gitpulse_lib::engine::repo_op;
use gitpulse_lib::engine::stash::{self, StashAction};
use gitpulse_lib::engine::submodules::{self, SubmoduleChange, SubmoduleState};
use gitpulse_lib::engine::{remotes, GitWriter, RemoteChange, ResetMode};
use std::fs;
use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

fn run_git(cwd: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn try_git(cwd: &Path, args: &[&str]) {
    let _ = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .env("GIT_EDITOR", "true")
        .env("GIT_SEQUENCE_EDITOR", "true")
        .env("GIT_AUTHOR_NAME", "T")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "T")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
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
    // Windows installs git with core.autocrlf=true in its system config, so a
    // fixture repo inherits it and git rewrites LF to CRLF on checkout --
    // silently breaking every assertion below that compares exact bytes.
    // These tests own their line endings; pin the policy rather than
    // inherit the host's.
    run_git(dir.path(), &["config", "core.autocrlf", "false"]);
    dir
}

fn commit(dir: &Path, file: &str, content: &str, message: &str) {
    fs::write(dir.join(file), content).unwrap();
    run_git(dir, &["add", "-A"]);
    run_git(dir, &["commit", "-m", message]);
}

/// Approves whatever argv it is given, recording it.
fn allow(argv: &[&str]) -> Result<Vec<String>, String> {
    Ok(argv.iter().map(|a| a.to_string()).collect())
}

// --- stash --------------------------------------------------------------

#[test]
fn lists_the_stash_stack_newest_first_with_branches_and_messages() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");

    fs::write(dir.join("f.txt"), "first\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "first WIP"]);
    run_git(dir, &["checkout", "-b", "feature"]);
    fs::write(dir.join("f.txt"), "second\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "second WIP"]);

    let entries = stash::list(&dir.to_string_lossy()).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].index, 0);
    assert_eq!(entries[0].message, "second WIP");
    assert_eq!(entries[0].branch.as_deref(), Some("feature"));
    assert_eq!(entries[1].index, 1);
    assert_eq!(entries[1].message, "first WIP");
    assert_eq!(entries[1].branch.as_deref(), Some("main"));
    assert_ne!(entries[0].oid, entries[1].oid);
}

#[test]
fn an_empty_stack_lists_as_empty() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    assert!(stash::list(&repo.path().to_string_lossy())
        .unwrap()
        .is_empty());
}

#[test]
fn applying_a_stash_restores_the_changes_and_keeps_the_entry() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "stashed\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "wip"]);

    let entries = stash::list(&dir.to_string_lossy()).unwrap();
    let entry = &entries[0];
    stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Apply,
        entry.index,
        &entry.oid,
        allow,
    )
    .unwrap();

    assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "stashed\n");
    assert_eq!(
        stash::list(&dir.to_string_lossy()).unwrap().len(),
        1,
        "apply must leave the entry on the stack"
    );
}

#[test]
fn popping_a_stash_restores_the_changes_and_removes_the_entry() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "stashed\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "wip"]);

    let entry = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();
    stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Pop,
        entry.index,
        &entry.oid,
        allow,
    )
    .unwrap();

    assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "stashed\n");
    assert!(stash::list(&dir.to_string_lossy()).unwrap().is_empty());
}

/// The regression the module exists for: the stack moved under the caller, so
/// the index it holds now names a different entry. Acting on it would drop
/// work the user never saw.
#[test]
fn a_stale_index_is_refused_rather_than_dropping_the_wrong_entry() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");

    fs::write(dir.join("f.txt"), "original\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "the one the user saw"]);
    let stale = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();
    assert_eq!(stale.index, 0);

    // Something else pushes a stash: every index shifts by one.
    fs::write(dir.join("f.txt"), "interloper\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "someone else's work"]);

    let err = stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Drop,
        stale.index,
        &stale.oid,
        allow,
    )
    .unwrap_err();
    assert!(
        err.contains("Refresh the stash list"),
        "refusal must tell the caller what to do, got: {err}"
    );

    // Both entries survive: nothing was destroyed.
    let after = stash::list(&dir.to_string_lossy()).unwrap();
    assert_eq!(after.len(), 2);
    assert!(after.iter().any(|e| e.oid == stale.oid));
}

#[test]
fn an_index_past_the_end_of_the_stack_is_refused() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "x\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "only"]);
    let entry = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();

    let err = stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Drop,
        7,
        &entry.oid,
        allow,
    )
    .unwrap_err();
    assert!(err.contains("no longer exists"), "got: {err}");
    assert_eq!(stash::list(&dir.to_string_lossy()).unwrap().len(), 1);
}

#[test]
fn a_refused_gate_leaves_the_stash_stack_untouched() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "x\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "only"]);
    let entry = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();

    let err = stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Drop,
        entry.index,
        &entry.oid,
        |_argv| Err::<(), String>("blocked by policy".into()),
    )
    .unwrap_err();
    assert_eq!(err, "blocked by policy");
    assert_eq!(stash::list(&dir.to_string_lossy()).unwrap().len(), 1);
}

#[test]
fn the_gate_sees_the_selector_for_destructive_verbs_and_the_oid_for_apply() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "x\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "only"]);
    let entry = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();

    let (seen, _) = stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Apply,
        entry.index,
        &entry.oid,
        allow,
    )
    .unwrap();
    assert_eq!(seen, vec!["git", "stash", "apply", entry.oid.as_str()]);

    let (seen, _) = stash::run_action_with(
        &dir.to_string_lossy(),
        StashAction::Drop,
        entry.index,
        &entry.oid,
        allow,
    )
    .unwrap();
    assert_eq!(seen, vec!["git", "stash", "drop", "stash@{0}"]);
}

#[test]
fn showing_a_stash_renders_its_diff() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    fs::write(dir.join("f.txt"), "changed\n").unwrap();
    run_git(dir, &["stash", "push", "-u", "-m", "wip"]);
    let entry = stash::list(&dir.to_string_lossy()).unwrap()[0].clone();

    let diff = stash::show(&dir.to_string_lossy(), &entry.oid).unwrap();
    assert!(diff.contains("f.txt"), "diff must name the file: {diff}");
    assert!(
        diff.contains("changed"),
        "diff must show the change: {diff}"
    );
}

#[test]
fn showing_a_stash_refuses_anything_that_is_not_an_object_id() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    for oid in ["--upload-pack=id", "stash@{0}", "", "../etc/passwd"] {
        assert!(
            stash::show(&repo.path().to_string_lossy(), oid).is_err(),
            "{oid:?} must be refused"
        );
    }
}

// --- remotes ------------------------------------------------------------

#[test]
fn a_repository_with_no_remotes_lists_empty_rather_than_erroring() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    assert!(remotes::list(&repo.path().to_string_lossy())
        .unwrap()
        .remotes
        .is_empty());
}

#[test]
fn adds_lists_renames_and_removes_a_remote() {
    let repo = init_repo();
    let dir = repo.path();
    let path = dir.to_string_lossy().into_owned();
    commit(dir, "f.txt", "a\n", "c1");

    remotes::apply_with(
        &path,
        &RemoteChange::Add {
            name: "origin".into(),
            url: "https://example.test/a.git".into(),
        },
        allow,
    )
    .unwrap();

    let listed = remotes::list(&path).unwrap();
    assert!(!listed.truncated);
    assert_eq!(listed.remotes.len(), 1);
    assert_eq!(listed.remotes[0].name, "origin");
    assert_eq!(
        listed.remotes[0].fetch_url.as_deref(),
        Some("https://example.test/a.git")
    );
    assert!(listed.remotes[0].is_default);
    assert_eq!(listed.remotes[0].push_url, None);

    remotes::apply_with(
        &path,
        &RemoteChange::Rename {
            name: "origin".into(),
            new_name: "upstream".into(),
        },
        allow,
    )
    .unwrap();
    assert_eq!(remotes::list(&path).unwrap().remotes[0].name, "upstream");

    remotes::apply_with(
        &path,
        &RemoteChange::Remove {
            name: "upstream".into(),
        },
        allow,
    )
    .unwrap();
    assert!(remotes::list(&path).unwrap().remotes.is_empty());
}

#[test]
fn a_separate_push_url_is_reported_alongside_the_fetch_url() {
    let repo = init_repo();
    let dir = repo.path();
    let path = dir.to_string_lossy().into_owned();
    commit(dir, "f.txt", "a\n", "c1");
    run_git(
        dir,
        &["remote", "add", "origin", "https://example.test/a.git"],
    );

    remotes::apply_with(
        &path,
        &RemoteChange::SetUrl {
            name: "origin".into(),
            url: "ssh://git@example.test/fork.git".into(),
            push: true,
        },
        allow,
    )
    .unwrap();

    let listed = remotes::list(&path).unwrap();
    assert_eq!(
        listed.remotes[0].fetch_url.as_deref(),
        Some("https://example.test/a.git")
    );
    assert_eq!(
        listed.remotes[0].push_url.as_deref(),
        Some("ssh://git@example.test/fork.git"),
        "a redirected push must be visible, not folded into the fetch URL"
    );
}

#[test]
fn adding_a_remote_that_already_exists_is_refused_by_name() {
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();
    commit(repo.path(), "f.txt", "a\n", "c1");
    run_git(
        repo.path(),
        &["remote", "add", "origin", "https://example.test/a.git"],
    );

    let err = remotes::apply_with(
        &path,
        &RemoteChange::Add {
            name: "origin".into(),
            url: "https://example.test/b.git".into(),
        },
        allow,
    )
    .unwrap_err();
    assert!(err.contains("already exists"), "got: {err}");
    // The original URL survives the refusal.
    assert_eq!(
        remotes::list(&path).unwrap().remotes[0]
            .fetch_url
            .as_deref(),
        Some("https://example.test/a.git")
    );
}

#[test]
fn acting_on_a_remote_that_does_not_exist_is_refused() {
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();
    commit(repo.path(), "f.txt", "a\n", "c1");
    let err = remotes::apply_with(
        &path,
        &RemoteChange::Remove {
            name: "ghost".into(),
        },
        allow,
    )
    .unwrap_err();
    assert!(err.contains("No remote named"), "got: {err}");
}

#[test]
fn a_hostile_remote_url_never_reaches_git() {
    let repo = init_repo();
    let path = repo.path().to_string_lossy().into_owned();
    commit(repo.path(), "f.txt", "a\n", "c1");

    for url in [
        "ext::sh -c 'touch /tmp/gitpulse-remote-pwn'",
        "--upload-pack=touch /tmp/x",
    ] {
        let err = remotes::apply_with(
            &path,
            &RemoteChange::Add {
                name: "evil".into(),
                url: url.into(),
            },
            allow,
        )
        .unwrap_err();
        assert!(
            !err.contains("already exists"),
            "must fail validation, got: {err}"
        );
    }
    assert!(!Path::new("/tmp/gitpulse-remote-pwn").exists());
    assert!(remotes::list(&path).unwrap().remotes.is_empty());
}

#[test]
fn tracking_branch_counts_reflect_real_remote_refs() {
    let origin = init_repo();
    commit(origin.path(), "f.txt", "a\n", "c1");
    run_git(origin.path(), &["branch", "feature"]);

    let clone_dir = TempDir::new().unwrap();
    let target = clone_dir.path().join("clone");
    run_git(
        clone_dir.path(),
        &[
            "clone",
            &origin.path().to_string_lossy(),
            &target.to_string_lossy(),
        ],
    );

    let listed = remotes::list(&target.to_string_lossy()).unwrap();
    assert_eq!(listed.remotes.len(), 1);
    assert_eq!(listed.remotes[0].name, "origin");
    assert!(
        listed.remotes[0].tracking_branches >= 2,
        "expected main and feature tracked, got {}",
        listed.remotes[0].tracking_branches
    );
}

// --- submodules ---------------------------------------------------------

/// Builds a superproject with one submodule and returns both temp dirs so
/// neither is dropped while the test runs.
fn repo_with_submodule() -> (TempDir, TempDir) {
    let lib = init_repo();
    commit(lib.path(), "l.txt", "lib\n", "libbase");

    let main = init_repo();
    commit(main.path(), "a.txt", "a\n", "base");
    run_git(
        main.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            &lib.path().to_string_lossy(),
            "vendor/lib",
        ],
    );
    run_git(main.path(), &["commit", "-m", "add submodule"]);
    (main, lib)
}

#[test]
fn a_repository_without_submodules_lists_empty() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    assert!(submodules::list(&repo.path().to_string_lossy())
        .unwrap()
        .submodules
        .is_empty());
}

#[test]
fn lists_a_submodule_with_its_configured_url_and_state() {
    let (main, lib) = repo_with_submodule();
    let listed = submodules::list(&main.path().to_string_lossy()).unwrap();
    assert_eq!(listed.submodules.len(), 1);
    assert_eq!(listed.submodules[0].path, "vendor/lib");
    assert_eq!(listed.submodules[0].state, SubmoduleState::UpToDate);
    assert!(!listed.submodules[0].orphaned);
    assert_eq!(
        listed.submodules[0].url.as_deref(),
        Some(lib.path().to_string_lossy().as_ref())
    );
}

/// The state a user actually hits: a fresh clone whose submodule folders are
/// empty and whose build fails for no visible reason.
#[test]
fn a_fresh_clone_reports_its_submodule_as_uninitialized() {
    let (main, _lib) = repo_with_submodule();
    let host = TempDir::new().unwrap();
    let target = host.path().join("clone");
    run_git(
        host.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            &main.path().to_string_lossy(),
            &target.to_string_lossy(),
        ],
    );

    let listed = submodules::list(&target.to_string_lossy()).unwrap();
    assert_eq!(listed.submodules.len(), 1);
    assert_eq!(listed.submodules[0].state, SubmoduleState::Uninitialized);
    assert!(listed.submodules[0].state.needs_attention());
    assert!(
        !listed.submodules[0].orphaned,
        "it is in .gitmodules, so it is fixable"
    );
}

/// Detection follows a submodule across initialization: uninitialized before,
/// up to date after.
///
/// The initialization itself is driven by `git` directly with
/// `-c protocol.file.allow=always`. That is not a workaround for a defect —
/// git refuses `file` transports for submodules by default (CVE-2022-39253),
/// and honours the override ONLY from the command line or the environment,
/// never from repository config, so a malicious repository cannot re-enable
/// it. GitPulse strips the `GIT_CONFIG_*` environment channel on purpose, so
/// its own update path cannot clone a local-path submodule either. Real
/// submodules use https/ssh and are unaffected; the next test pins that this
/// refusal is reported honestly rather than swallowed.
#[test]
fn detection_follows_a_submodule_across_initialization() {
    let (main, _lib) = repo_with_submodule();
    let host = TempDir::new().unwrap();
    let target = host.path().join("clone");
    run_git(
        host.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            &main.path().to_string_lossy(),
            &target.to_string_lossy(),
        ],
    );

    let path = target.to_string_lossy().into_owned();
    assert_eq!(
        submodules::list(&path).unwrap().submodules[0].state,
        SubmoduleState::Uninitialized
    );

    run_git(
        &target,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "update",
            "--init",
        ],
    );

    let listed = submodules::list(&path).unwrap();
    assert_eq!(listed.submodules[0].state, SubmoduleState::UpToDate);
    assert!(!listed.submodules[0].state.needs_attention());
    assert!(target.join("vendor/lib/l.txt").exists());
}

/// A submodule update that git refuses must surface the refusal, not report
/// success over an empty directory.
#[test]
fn a_refused_submodule_transport_is_reported_rather_than_swallowed() {
    let (main, _lib) = repo_with_submodule();
    let host = TempDir::new().unwrap();
    let target = host.path().join("clone");
    run_git(
        host.path(),
        &[
            "-c",
            "protocol.file.allow=always",
            "clone",
            &main.path().to_string_lossy(),
            &target.to_string_lossy(),
        ],
    );

    let path = target.to_string_lossy().into_owned();
    let err = submodules::apply_with(
        &path,
        &SubmoduleChange::Update {
            path: None,
            recursive: false,
        },
        allow,
    )
    .unwrap_err();
    assert!(
        err.contains("transport 'file' not allowed"),
        "the real cause must reach the caller, got: {err}"
    );
    // And the submodule is still honestly reported as uninitialized.
    assert_eq!(
        submodules::list(&path).unwrap().submodules[0].state,
        SubmoduleState::Uninitialized
    );
}

#[test]
fn a_submodule_checked_out_elsewhere_reports_as_moved() {
    let (main, _lib) = repo_with_submodule();
    let sub_dir = main.path().join("vendor/lib");
    commit(&sub_dir, "l.txt", "moved\n", "moved");

    let listed = submodules::list(&main.path().to_string_lossy()).unwrap();
    assert_eq!(listed.submodules[0].state, SubmoduleState::CommitDiffers);
    assert!(listed.submodules[0].state.needs_attention());
}

#[test]
fn naming_a_path_that_is_not_a_submodule_is_refused() {
    let (main, _lib) = repo_with_submodule();
    let err = submodules::apply_with(
        &main.path().to_string_lossy(),
        &SubmoduleChange::Update {
            path: Some("not/a/submodule".into()),
            recursive: false,
        },
        allow,
    )
    .unwrap_err();
    assert!(err.contains("No submodule at"), "got: {err}");
}

#[test]
fn a_hostile_submodule_path_never_reaches_git() {
    let (main, _lib) = repo_with_submodule();
    for path in ["../../escape", "--exec=sh", "/etc"] {
        assert!(
            submodules::apply_with(
                &main.path().to_string_lossy(),
                &SubmoduleChange::Deinit {
                    path: path.into(),
                    force: true,
                },
                allow,
            )
            .is_err(),
            "path {path:?} must be refused"
        );
    }
}

// --- cherry-pick, revert, reset -----------------------------------------

#[test]
fn cherry_picks_a_commit_onto_the_current_branch() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "a.txt", "a\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "b.txt", "b\n", "add b");
    let picked = git_out(dir, &["rev-parse", "HEAD"]);
    run_git(dir, &["checkout", "main"]);

    GitWriter::cherry_pick(&dir.to_string_lossy(), &[picked], false).unwrap();
    assert!(dir.join("b.txt").exists());
    assert_eq!(
        repo_op::detect(dir).unwrap(),
        None,
        "a clean pick parks nothing"
    );
}

#[test]
fn a_conflicting_cherry_pick_parks_the_repository_for_the_recovery_banner() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "f.txt", "side\n", "side");
    let picked = git_out(dir, &["rev-parse", "HEAD"]);
    run_git(dir, &["checkout", "main"]);
    commit(dir, "f.txt", "main\n", "main");

    let err = GitWriter::cherry_pick(&dir.to_string_lossy(), &[picked], false).unwrap_err();
    assert!(!err.is_empty());
    // The parked state is the supported outcome — the banner offers the exit.
    let parked = repo_op::detect(dir).unwrap().expect("must be parked");
    assert_eq!(parked.kind, repo_op::OperationKind::CherryPick);
}

#[test]
fn reverting_a_commit_undoes_its_change() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "v1\n", "c1");
    commit(dir, "f.txt", "v2\n", "c2");
    let target = git_out(dir, &["rev-parse", "HEAD"]);

    GitWriter::revert(&dir.to_string_lossy(), &[target], false).unwrap();
    assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "v1\n");
}

#[test]
fn a_replay_refuses_to_start_on_top_of_a_parked_operation() {
    // Starting a second sequencer over the first does not queue behind it; the
    // refusal must name the operation the user can actually see and abort.
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "f.txt", "side\n", "side");
    let picked = git_out(dir, &["rev-parse", "HEAD"]);
    run_git(dir, &["checkout", "main"]);
    commit(dir, "f.txt", "main\n", "main");
    try_git(dir, &["merge", "--no-edit", "side"]);

    let err = GitWriter::cherry_pick(&dir.to_string_lossy(), &[picked], false).unwrap_err();
    assert!(err.contains("merge is in progress"), "got: {err}");
    // The merge is untouched.
    assert_eq!(
        repo_op::detect(dir).unwrap().unwrap().kind,
        repo_op::OperationKind::Merge
    );
}

#[test]
fn an_empty_commit_list_is_refused() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    let err = GitWriter::cherry_pick(&repo.path().to_string_lossy(), &[], false).unwrap_err();
    assert!(err.contains("No commits"), "got: {err}");
}

#[test]
fn a_hostile_revision_never_reaches_git() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    for rev in ["--upload-pack=id", "a..b", "HEAD;id", "-x"] {
        assert!(
            GitWriter::cherry_pick(&repo.path().to_string_lossy(), &[rev.to_string()], false)
                .is_err(),
            "revision {rev:?} must be refused"
        );
    }
}

#[test]
fn a_soft_reset_moves_the_branch_and_keeps_the_working_tree() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "v1\n", "c1");
    commit(dir, "f.txt", "v2\n", "c2");

    GitWriter::reset(&dir.to_string_lossy(), ResetMode::Soft, "HEAD~1").unwrap();
    assert_eq!(git_out(dir, &["log", "--format=%s", "-1"]), "c1");
    assert_eq!(
        fs::read_to_string(dir.join("f.txt")).unwrap(),
        "v2\n",
        "a soft reset must not touch the working tree"
    );
}

#[test]
fn a_hard_reset_discards_the_working_tree_as_advertised() {
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "v1\n", "c1");
    commit(dir, "f.txt", "v2\n", "c2");
    fs::write(dir.join("f.txt"), "uncommitted\n").unwrap();

    GitWriter::reset(&dir.to_string_lossy(), ResetMode::Hard, "HEAD~1").unwrap();
    assert_eq!(fs::read_to_string(dir.join("f.txt")).unwrap(), "v1\n");
    assert!(git_out(dir, &["status", "--porcelain"]).is_empty());
    assert!(ResetMode::Hard.discards_working_tree());
    assert!(!ResetMode::Soft.discards_working_tree());
}

#[test]
fn a_reset_refuses_while_an_operation_is_parked() {
    // A reset mid-merge abandons the merge's state instead of ending it; the
    // user wants abort, which the banner offers.
    let repo = init_repo();
    let dir = repo.path();
    commit(dir, "f.txt", "base\n", "base");
    run_git(dir, &["checkout", "-b", "side"]);
    commit(dir, "f.txt", "side\n", "side");
    run_git(dir, &["checkout", "main"]);
    commit(dir, "f.txt", "main\n", "main");
    try_git(dir, &["merge", "--no-edit", "side"]);

    let err = GitWriter::reset(&dir.to_string_lossy(), ResetMode::Hard, "HEAD").unwrap_err();
    assert!(err.contains("merge is in progress"), "got: {err}");
    assert!(repo_op::detect(dir).unwrap().is_some());
}

#[test]
fn a_hostile_reset_target_never_reaches_git() {
    let repo = init_repo();
    commit(repo.path(), "f.txt", "a\n", "c1");
    for target in ["--hard", "a..b", "HEAD;id"] {
        assert!(
            GitWriter::reset(&repo.path().to_string_lossy(), ResetMode::Soft, target).is_err(),
            "target {target:?} must be refused"
        );
    }
}

#[test]
fn every_reset_mode_renders_the_flag_it_names() {
    for (mode, flag) in [
        (ResetMode::Soft, "--soft"),
        (ResetMode::Mixed, "--mixed"),
        (ResetMode::Keep, "--keep"),
        (ResetMode::Hard, "--hard"),
    ] {
        assert_eq!(
            GitWriter::reset_argv(mode, "HEAD"),
            vec!["git", "reset", flag, "HEAD"]
        );
    }
}
