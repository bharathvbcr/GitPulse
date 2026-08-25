//! End-to-end tests against a real `manvi serve` sidecar and a real repository.
//!
//! The policy tests need the MANVI binary. When it is absent they report a skip
//! on stderr rather than passing quietly — a check that could not run must not
//! look like one that ran and passed. Set `GITPULSE_REQUIRE_MANVI=1` (as CI
//! should) to turn its absence into a failure.

use std::process::Command;

use gitpulse_lib::graph::{list_ref_decorations, RefKind};
use gitpulse_lib::harness::{check_command, check_file, HarnessStatus, PolicyStatus};
use tempfile::TempDir;

fn git(dir: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .env("GIT_AUTHOR_NAME", "Test")
        .env("GIT_AUTHOR_EMAIL", "test@example.com")
        .env("GIT_COMMITTER_NAME", "Test")
        .env("GIT_COMMITTER_EMAIL", "test@example.com")
        .output()
        .unwrap_or_else(|e| panic!("git {:?} failed to spawn: {}", args, e));
    assert!(
        status.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&status.stderr)
    );
}

/// A repository with one commit on `main`, a `feature` branch and a `v1.0` tag.
fn fixture() -> TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path();
    git(path, &["init", "--initial-branch=main", "."]);
    std::fs::write(path.join("a.txt"), "hello\n").unwrap();
    git(path, &["add", "a.txt"]);
    git(path, &["commit", "-m", "feat: first"]);
    git(path, &["branch", "feature"]);
    git(path, &["tag", "-a", "v1.0", "-m", "release one"]);
    dir
}

fn repo_path(dir: &TempDir) -> String {
    dir.path()
        .canonicalize()
        .expect("canonical repo path")
        .to_string_lossy()
        .into_owned()
}

#[test]
fn ref_decorations_name_branches_tags_and_head() {
    let dir = fixture();
    let refs = list_ref_decorations(&repo_path(&dir)).expect("refs");

    let head = refs
        .iter()
        .find(|r| r.is_head)
        .expect("some ref is marked HEAD");
    assert_eq!(head.name, "main");
    assert_eq!(head.kind, RefKind::Local);
    // HEAD sorts first so the chip nearest the node is the one you are on.
    assert!(refs[0].is_head);

    let feature = refs
        .iter()
        .find(|r| r.name == "feature")
        .expect("feature branch");
    assert_eq!(feature.kind, RefKind::Local);
    assert!(!feature.is_head);

    // An annotated tag must resolve to the commit it peels to, not to the tag
    // object, or it would decorate no row at all.
    let tag = refs.iter().find(|r| r.name == "v1.0").expect("tag");
    assert_eq!(tag.kind, RefKind::Tag);
    assert_eq!(tag.commit_id, head.commit_id);
}

#[test]
fn detached_head_is_still_reported() {
    let dir = fixture();
    let head = String::from_utf8(
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .current_dir(dir.path())
            .output()
            .unwrap()
            .stdout,
    )
    .unwrap();
    git(dir.path(), &["checkout", "--detach", head.trim()]);

    let refs = list_ref_decorations(&repo_path(&dir)).expect("refs");
    let marked = refs
        .iter()
        .find(|r| r.is_head)
        .expect("detached HEAD is marked");
    assert_eq!(marked.kind, RefKind::Head);
    assert_eq!(marked.commit_id, head.trim());
}

/// True when the MANVI binary is present; reports a skip when it is not.
fn manvi_available(test: &str) -> bool {
    let status = HarnessStatus::probe();
    if status.available {
        return true;
    }
    if std::env::var("GITPULSE_REQUIRE_MANVI").as_deref() == Ok("1") {
        panic!(
            "{} requires the MANVI harness and it is unavailable: {}",
            test, status.error
        );
    }
    eprintln!(
        "SKIPPED {}: MANVI harness unavailable ({}). This check did not run.",
        test, status.error
    );
    false
}

#[test]
fn harness_refuses_the_commands_it_exists_to_refuse() {
    if !manvi_available("harness_refuses_the_commands_it_exists_to_refuse") {
        return;
    }
    let dir = fixture();
    let root = repo_path(&dir);

    let force_push = check_command(&root, "git push --force origin main");
    assert!(force_push.checked);
    assert_eq!(force_push.status, PolicyStatus::Blocked);
    assert_eq!(force_push.rule, "command.force_push");
    assert_eq!(force_push.severity, "hard");

    let bypass = check_command(&root, "git commit --no-verify -m x");
    assert_eq!(bypass.status, PolicyStatus::Blocked);
    assert_eq!(bypass.rule, "command.bypass_flag");

    // An ordinary commit is allowed — but under the host posture it is a
    // demoted allow, and the verdict must say so rather than reading clean.
    let commit = check_command(&root, "git commit -m 'feat: add a thing'");
    assert!(!commit.blocks());
    assert_eq!(commit.status, PolicyStatus::Demoted);
    assert!(!commit.demoted.is_empty());

    // Secret paths are refused whatever the host thinks it is doing.
    let secret = check_file(&root, ".env", "modify");
    assert_eq!(secret.status, PolicyStatus::Blocked);
    assert_eq!(secret.rule, "path.secret");

    let ordinary = check_file(&root, "src/main.rs", "modify");
    assert!(!ordinary.blocks());
}

#[test]
fn the_sidecar_never_writes_into_the_users_repository() {
    if !manvi_available("the_sidecar_never_writes_into_the_users_repository") {
        return;
    }
    let dir = fixture();
    let root = repo_path(&dir);
    std::fs::write(dir.path().join(".gitignore"), "node_modules/\n").unwrap();

    // Every MANVI command prepares the repository it stands in: it creates a
    // state directory and appends managed rules to .gitignore. A Git client
    // must not do that to a repository the user merely opened, so the sidecar
    // is started elsewhere with initialisation disabled. This is the check that
    // the arrangement actually holds.
    let _ = check_command(&root, "git status");
    let _ = check_file(&root, "src/main.rs", "modify");

    assert_eq!(
        std::fs::read_to_string(dir.path().join(".gitignore")).unwrap(),
        "node_modules/\n",
        "the harness rewrote the repository's .gitignore"
    );
    assert!(
        !dir.path().join(".devcouncil").exists(),
        "the harness created a state directory inside the repository"
    );

    let dirty = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&dirty.stdout).trim(),
        ".gitignore"
            .to_string()
            .replace(".gitignore", "?? .gitignore"),
        "the working tree changed in some way other than the .gitignore this test wrote"
    );
}
