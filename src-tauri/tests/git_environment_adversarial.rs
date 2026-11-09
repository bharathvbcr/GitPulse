//! Adversarial checks for the git environment pins (lane W3-L3): the locale pin
//! must still hand the parsers English under a translated parent, and the
//! textconv and identity rules must hold on every path that can reach them.

mod common;
use common::{run_git, trust_repo};
use gitpulse_lib::engine::git_writer::MergeMode;
use gitpulse_lib::engine::{GitReader, GitWriter};
use gitpulse_lib::graph::RefScope;
use gitpulse_lib::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command;

const BREW_GIT_DIR: &str = "/opt/homebrew/bin";

fn repo() -> tempfile::TempDir {
    common::unmetered_spawns();
    let dir = tempfile::tempdir().expect("tempdir");
    run_git(dir.path(), &["init", "-q", "-b", "main"]);
    run_git(dir.path(), &["config", "commit.gpgsign", "false"]);
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    dir
}

fn commit_file(dir: &Path, name: &str, body: &str) {
    std::fs::write(dir.join(name), body).expect("write fixture");
    run_git(dir, &["add", "--", name]);
    run_git(dir, &["commit", "-q", "-m", name]);
}

/// The PATH that puts a gettext-enabled git with a German catalog first, or
/// `None` when this machine has none (English is then inferred, not verified).
fn translating_git_path() -> Option<String> {
    let git = Path::new(BREW_GIT_DIR).join("git");
    if !git.exists() {
        return None;
    }
    let probe = tempfile::tempdir().ok()?;
    let out = Command::new(&git)
        .arg("status")
        .current_dir(probe.path())
        .env_remove("LC_ALL")
        .env("GIT_CEILING_DIRECTORIES", probe.path().parent()?)
        .env("LANG", "de_DE.UTF-8")
        .env("LC_MESSAGES", "de_DE.UTF-8")
        .env("LANGUAGE", "de")
        .output_locked()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stderr);
    (!text.contains("not a git repository")).then(|| {
        format!(
            "{BREW_GIT_DIR}:{}",
            std::env::var("PATH").unwrap_or_default()
        )
    })
}

/// With the parent speaking German (LANG, LC_MESSAGES and LANGUAGE all set), the
/// parsers must still read English: a conflict is reported as `CONFLICT`, and an
/// unborn HEAD reads as an empty history rather than an error.
#[cfg(unix)]
#[test]
fn parsers_read_english_under_a_german_parent() {
    let serial = common::env::env_serial();
    let Some(path) = translating_git_path() else {
        eprintln!("no translating git on this machine; English is inferred only");
        return;
    };
    let dir = repo();
    let _env = common::env::bind_env(&serial)
        .set("PATH", &path)
        .set("LANG", "de_DE.UTF-8")
        .set("LC_MESSAGES", "de_DE.UTF-8")
        .set("LANGUAGE", "de")
        .remove("LC_ALL");
    let p = dir.path().to_str().unwrap();
    trust_repo(dir.path());

    let unborn = GitReader::read_commit_history_paged(p, 0, 10, None, None, RefScope::Named);
    assert!(
        unborn.as_ref().is_ok_and(|v| v.is_empty()),
        "unborn HEAD was not recognised under a German parent: {unborn:?}"
    );

    commit_file(dir.path(), "a.txt", "base\n");
    run_git(dir.path(), &["checkout", "-q", "-b", "side"]);
    commit_file(dir.path(), "a.txt", "side\n");
    run_git(dir.path(), &["checkout", "-q", "main"]);
    commit_file(dir.path(), "a.txt", "main\n");
    let err = GitWriter::merge_branch(p, "side", MergeMode::Default, &mut |_| Ok(()))
        .expect_err("conflicting merge");
    assert!(
        err.message().contains("CONFLICT"),
        "merge conflict text was not English: {}",
        err.message()
    );
}

/// `git stash show -p` prints a patch through the same diff machinery, so a
/// `diff.<driver>.textconv` runs on it unless `--no-textconv` is passed.
#[cfg(unix)]
#[test]
fn stash_show_never_runs_a_textconv_driver() {
    use std::os::unix::fs::PermissionsExt;
    let _serial = common::env::env_serial();
    let dir = repo();
    let tools = tempfile::tempdir().expect("tools");
    let marker = tools.path().join("ran");
    let converter = tools.path().join("convert.sh");
    std::fs::write(
        &converter,
        format!("#!/bin/sh\n: > '{}'\necho CONVERTED\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&converter, std::fs::Permissions::from_mode(0o755)).unwrap();
    run_git(
        dir.path(),
        &["config", "diff.x.textconv", &converter.display().to_string()],
    );
    std::fs::write(dir.path().join(".gitattributes"), "*.bin diff=x\n").unwrap();
    run_git(dir.path(), &["add", "--", ".gitattributes"]);
    run_git(dir.path(), &["commit", "-q", "-m", "attrs"]);
    commit_file(dir.path(), "a.bin", "one\n");
    std::fs::write(dir.path().join("a.bin"), "one\ntwo\n").unwrap();
    run_git(dir.path(), &["stash", "push", "-q"]);
    let oid = Command::new("git")
        .args(["rev-parse", "refs/stash"])
        .current_dir(dir.path())
        .output_locked()
        .expect("rev-parse");
    let oid = String::from_utf8_lossy(&oid.stdout).trim().to_string();
    trust_repo(dir.path());

    let shown = gitpulse_lib::engine::stash::show(dir.path().to_str().unwrap(), &oid)
        .expect("stash show");
    assert!(!marker.exists(), "the textconv driver ran for stash show");
    assert!(
        !shown.text.contains("CONVERTED") && shown.text.contains("+two"),
        "stash patch is converter text: {:?}",
        shown.text
    );
}

fn git_var_author(dir: &Path, env: &[(&str, &str)]) -> (bool, String) {
    let mut cmd = Command::new("git");
    cmd.args(["var", "GIT_AUTHOR_IDENT"])
        .current_dir(dir)
        .env_remove("EMAIL")
        .env_remove("GIT_COMMITTER_NAME")
        .env_remove("GIT_COMMITTER_EMAIL")
        .env_remove("GIT_AUTHOR_NAME")
        .env_remove("GIT_AUTHOR_EMAIL");
    for (k, v) in env {
        cmd.env(k, v);
    }
    let out = cmd.output_locked().expect("git var");
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).trim().to_string(),
    )
}

/// git >= 2.22 resolves `author.name` / `author.email` ahead of `user.*` for the
/// author ident.
#[test]
fn identity_honours_author_config_keys_like_git_var() {
    let serial = common::env::env_serial();
    let dir = repo();
    let _env = common::env::bind_env(&serial)
        .remove("EMAIL")
        .remove("GIT_AUTHOR_NAME")
        .remove("GIT_AUTHOR_EMAIL")
        .remove("GIT_COMMITTER_NAME")
        .remove("GIT_COMMITTER_EMAIL");
    run_git(dir.path(), &["config", "author.name", "Author Cfg"]);
    run_git(dir.path(), &["config", "author.email", "author@example.com"]);
    trust_repo(dir.path());
    let (ok, line) = git_var_author(dir.path(), &[]);
    assert!(ok && line.starts_with("Author Cfg <author@example.com>"), "{line}");
    let id = GitWriter::identity(dir.path().to_str().unwrap()).expect("identity");
    assert_eq!(id.name.as_deref(), Some("Author Cfg"), "git var said: {line}");
    assert_eq!(id.email.as_deref(), Some("author@example.com"), "git var said: {line}");
}

/// An empty `GIT_AUTHOR_NAME` makes `git var` (and `git commit`) refuse, even
/// with `user.name` set; `identity()` must not report a complete identity for a
/// state in which git would not record a commit.
#[test]
fn identity_is_incomplete_when_git_refuses_an_empty_author_name() {
    let serial = common::env::env_serial();
    let dir = repo();
    let _env = common::env::bind_env(&serial)
        .remove("EMAIL")
        .set("GIT_AUTHOR_NAME", "")
        .remove("GIT_AUTHOR_EMAIL")
        .remove("GIT_COMMITTER_NAME")
        .remove("GIT_COMMITTER_EMAIL");
    trust_repo(dir.path());
    let (ok, line) = git_var_author(dir.path(), &[("GIT_AUTHOR_NAME", "")]);
    assert!(!ok, "git var unexpectedly accepted an empty name: {line}");
    let id = GitWriter::identity(dir.path().to_str().unwrap()).expect("identity");
    assert!(
        !id.is_complete(),
        "identity() is complete ({id:?}) where `git var` refuses"
    );
}
