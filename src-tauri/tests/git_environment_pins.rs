//! Git's output format and environment are pinned, whatever the developer's
//! own configuration and shell say.
//!
//! Each case sets a user-level setting that changes what git prints or what a
//! pathspec matches, runs the real engine against a real repository, and checks
//! what the parsers would have read. The settings live in the repository's own
//! config or in this process's environment, which is exactly where a developer
//! (or a CI image) puts them.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::engine::{GitReader, GitWriter};
use gitpulse_lib::graph::RefScope;
use std::path::Path;
use std::process::Command;
use std::sync::Mutex;

/// A test that edits this process's environment takes the turn, and so does
/// every other test: a spawned child inherits the edit.
static ENV: Mutex<()> = Mutex::new(());

fn repo() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    dir
}

fn commit_file(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write fixture");
    run_git(dir, &["add", "--", name]);
    run_git(dir, &["commit", "-q", "-m", name]);
}

fn head(dir: &Path) -> String {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .expect("git rev-parse");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn is_oid(id: &str) -> bool {
    matches!(id.len(), 40 | 64) && id.bytes().all(|b| b.is_ascii_hexdigit())
}

/// `log.showSignature=true` prints "No signature" (or the verification result)
/// ahead of the id of a signed commit, so a node id became
/// "No signature\n<sha>" and every child dangled. An SSH key keeps the signed
/// commit hermetic: no gpg, no keyring.
#[test]
fn log_showsignature_never_reaches_a_commit_id() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo();
    let key = dir.path().join("signing-key");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", ""])
        .arg("-f")
        .arg(&key)
        .output()
        .expect("ssh-keygen is required to sign the fixture commit");
    assert!(keygen.status.success(), "ssh-keygen failed");
    run_git(dir.path(), &["config", "gpg.format", "ssh"]);
    run_git(
        dir.path(),
        &[
            "config",
            "user.signingkey",
            &format!("{}.pub", key.display()),
        ],
    );
    std::fs::write(dir.path().join("a.txt"), "a").unwrap();
    run_git(dir.path(), &["add", "--", "a.txt"]);
    run_git(dir.path(), &["commit", "-q", "-S", "-m", "signed"]);
    commit_file(dir.path(), "b.txt", "b");
    run_git(dir.path(), &["config", "log.showSignature", "true"]);
    trust_repo(dir.path());
    let path = dir.path().to_str().unwrap();

    let page = GitReader::read_commit_history_paged(path, 0, 50, None, None, RefScope::Named)
        .expect("history page");
    assert!(page.len() >= 2, "expected both commits, got {}", page.len());
    for node in &page {
        assert!(
            is_oid(&node.id),
            "node id is not an object id: {:?}",
            node.id
        );
        assert!(
            node.parent_ids.iter().all(|parent| is_oid(parent)),
            "parent ids are not object ids: {:?}",
            node.parent_ids
        );
    }
    let details = GitReader::get_commit_details(path, &head(dir.path())).expect("details");
    assert!(is_oid(&details.id), "details id: {:?}", details.id);
}

/// `color.ui=always` put ANSI escapes in the diff viewer and `diff.external`
/// replaced git's diff with a program's output.
#[test]
fn colour_and_external_diff_settings_do_not_reach_the_diff() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo();
    commit_file(dir.path(), "a.txt", "one\n");
    std::fs::write(dir.path().join("a.txt"), "two\n").unwrap();
    for (key, value) in [
        ("color.ui", "always"),
        ("color.diff", "always"),
        ("diff.external", "/usr/bin/false"),
        ("diff.noprefix", "true"),
    ] {
        run_git(dir.path(), &["config", key, value]);
    }
    trust_repo(dir.path());
    let path = dir.path().to_str().unwrap();

    let unstaged = GitReader::get_file_diff(path, "a.txt", false, false).expect("diff");
    assert!(
        unstaged.text.starts_with("diff --git a/a.txt b/a.txt"),
        "diff did not start with git's own header: {:?}",
        unstaged.text.lines().next()
    );
    assert!(!unstaged.text.contains('\x1b'), "ANSI escapes in the diff");

    let committed = GitReader::get_commit_diff(path, &head(dir.path())).expect("commit diff");
    assert!(
        !committed.text.contains('\x1b'),
        "ANSI escapes in a commit diff"
    );
    assert!(
        committed.text.contains("diff --git a/a.txt b/a.txt"),
        "external diff or prefix settings changed the commit diff: {:?}",
        committed.text.lines().next()
    );
}

/// `GIT_LITERAL_PATHSPECS=1` in the parent turns the `:(literal)` magic the
/// writers prepend into literal text, so staging `f*.txt` failed or matched
/// something other than the file named.
#[test]
fn ambient_pathspec_switches_do_not_retarget_stage() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo();
    commit_file(dir.path(), "base.txt", "base");
    std::fs::write(dir.path().join("f*.txt"), "star").unwrap();
    std::fs::write(dir.path().join("fa.txt"), "other").unwrap();
    trust_repo(dir.path());
    let path = dir.path().to_str().unwrap();

    let names = [
        "GIT_LITERAL_PATHSPECS",
        "GIT_GLOB_PATHSPECS",
        "GIT_ICASE_PATHSPECS",
    ];
    for name in names {
        std::env::set_var(name, "1");
    }
    let staged = GitWriter::stage_file(path, "f*.txt");
    for name in names {
        std::env::remove_var(name);
    }
    staged.expect("stage the one file named f*.txt");

    let cached = Command::new("git")
        .args(["diff", "--cached", "--name-only", "-z"])
        .current_dir(dir.path())
        .output()
        .expect("git diff --cached");
    assert_eq!(
        String::from_utf8_lossy(&cached.stdout).trim_end_matches('\0'),
        "f*.txt",
        "staging touched something other than the file named"
    );
}

/// `%ad` honours `log.date`; a developer's `relative` would freeze "2 minutes
/// ago" into the commit panel.
#[test]
fn log_date_does_not_change_the_commit_dates() {
    let _turn = ENV.lock().unwrap_or_else(|e| e.into_inner());
    let dir = repo();
    commit_file(dir.path(), "a.txt", "a");
    run_git(dir.path(), &["config", "log.date", "relative"]);
    trust_repo(dir.path());

    let details = GitReader::get_commit_details(dir.path().to_str().unwrap(), &head(dir.path()))
        .expect("details");
    assert!(
        !details.author_date.contains("ago") && !details.committer_date.contains("ago"),
        "log.date reached the dates: {:?} / {:?}",
        details.author_date,
        details.committer_date
    );
}
