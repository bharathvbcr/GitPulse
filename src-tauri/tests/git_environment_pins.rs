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
use gitpulse_lib::engine::git_writer::MergeMode;
use gitpulse_lib::engine::{GitReader, GitWriter};
use gitpulse_lib::graph::RefScope;
use gitpulse_lib::procguard::LockedSpawn;
use std::path::Path;
use std::process::Command;

fn repo() -> tempfile::TempDir {
    common::unmetered_spawns();
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
        .output_locked()
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
    // Every test takes the environment turn: a spawned child inherits an edit.
    let _serial = common::env::env_serial();
    let dir = repo();
    let key = dir.path().join("signing-key");
    let keygen = Command::new("ssh-keygen")
        .args(["-q", "-t", "ed25519", "-N", ""])
        .arg("-f")
        .arg(&key)
        .output_locked()
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
    // Every test takes the environment turn: a spawned child inherits an edit.
    let _serial = common::env::env_serial();
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
    // Every test takes the environment turn: a spawned child inherits an edit.
    let serial = common::env::env_serial();
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
    let mut pins = common::env::bind_env(&serial);
    for name in names {
        pins = pins.set(name, "1");
    }
    let staged = GitWriter::stage_file(path, "f*.txt");
    drop(pins);
    staged.expect("stage the one file named f*.txt");

    let cached = Command::new("git")
        .args(["diff", "--cached", "--name-only", "-z"])
        .current_dir(dir.path())
        .output_locked()
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
    // Every test takes the environment turn: a spawned child inherits an edit.
    let _serial = common::env::env_serial();
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

/// Executable `body` (a `/bin/sh` script) written at `path`.
#[cfg(unix)]
fn write_script(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::write(path, body).expect("write script");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
}

/// A user's `diff.<driver>.textconv` ran on repository bytes for `diff`, `show`,
/// `log -p` and `blame`: the converter's text came back as the "patch" and as
/// the blamed lines. The converter here writes a marker file when it runs, so
/// the test sees both that it did not run and that git's own output came back.
#[cfg(unix)]
#[test]
fn textconv_drivers_never_run_for_diffs_or_blame() {
    // Every test takes the environment turn: a spawned child inherits an edit.
    let _serial = common::env::env_serial();
    let dir = repo();
    let tools = tempfile::tempdir().expect("tools dir");
    let marker = tools.path().join("textconv-ran");
    let converter = tools.path().join("convert.sh");
    write_script(
        &converter,
        &format!(
            "#!/bin/sh\n: > '{}'\necho CONVERTED-BY-TEXTCONV\n",
            marker.display()
        ),
    );
    run_git(
        dir.path(),
        &[
            "config",
            "diff.x.textconv",
            &converter.display().to_string(),
        ],
    );
    std::fs::write(dir.path().join(".gitattributes"), "*.bin diff=x\n").unwrap();
    run_git(dir.path(), &["add", "--", ".gitattributes"]);
    run_git(dir.path(), &["commit", "-q", "-m", "attributes"]);
    commit_file(dir.path(), "a.bin", "one\n");
    let first = head(dir.path());
    commit_file(dir.path(), "a.bin", "one\ntwo\n");
    let second = head(dir.path());
    std::fs::write(dir.path().join("a.bin"), "one\ntwo\nthree\n").unwrap();
    trust_repo(dir.path());
    let path = dir.path().to_str().unwrap();

    let worktree = GitReader::get_file_diff(path, "a.bin", false, false).expect("file diff");
    let commit = GitReader::get_commit_file_diff(path, &second, "a.bin").expect("commit file diff");
    let range = GitReader::get_range_diff(path, &first, &second).expect("range diff");
    let blame = GitReader::get_file_blame(path, "a.bin", None).expect("blame");
    let blame_at = GitReader::get_file_blame(path, "a.bin", Some(&second)).expect("blame at");

    assert!(!marker.exists(), "the textconv driver ran");
    for (label, text) in [
        ("working-tree diff", &worktree.text),
        ("commit file diff", &commit.text),
        ("range diff", &range.text),
    ] {
        assert!(
            !text.contains("CONVERTED-BY-TEXTCONV"),
            "{label} is converter text: {text:?}"
        );
        assert!(text.contains("@@"), "{label} is not a patch: {text:?}");
    }
    assert!(worktree.text.contains("+three"), "{:?}", worktree.text);
    assert!(commit.text.contains("+two"), "{:?}", commit.text);
    for lines in [&blame, &blame_at] {
        assert!(
            lines.iter().any(|l| l.content == "two"),
            "blame did not return the file's own lines: {lines:?}"
        );
        assert!(lines.iter().all(|l| l.content != "CONVERTED-BY-TEXTCONV"));
    }
}

/// The contents of the environment variables a hook saw, one `NAME=value` line
/// each (`<unset>` for a name the hook did not inherit).
#[cfg(unix)]
fn commit_and_record_hook_locale(dir: &Path) -> String {
    let record = dir.join(".git").join("hook-locale.txt");
    write_script(
        &dir.join(".git").join("hooks").join("pre-commit"),
        &format!(
            "#!/bin/sh\nprintf 'LC_ALL=%s\\nLC_CTYPE=%s\\nLANG=%s\\nLC_MESSAGES=%s\\nLANGUAGE=%s\\n' \
             \"${{LC_ALL-<unset>}}\" \"${{LC_CTYPE-<unset>}}\" \"${{LANG-<unset>}}\" \
             \"${{LC_MESSAGES-<unset>}}\" \"${{LANGUAGE-<unset>}}\" > '{}'\n",
            record.display()
        ),
    );
    run_git(dir, &["config", "user.name", "Test User"]);
    run_git(dir, &["config", "user.email", "test@example.com"]);
    std::fs::write(dir.join("hooked.txt"), "x\n").unwrap();
    run_git(dir, &["add", "--", "hooked.txt"]);
    trust_repo(dir);
    GitWriter::commit(dir.to_str().unwrap(), "from gitpulse", false).expect("commit");
    std::fs::read_to_string(&record).expect("the pre-commit hook ran and recorded its env")
}

/// A hook run from GitPulse's commit path sees the user's `LANG`, not
/// `LC_ALL=C` (which Ruby reads as US-ASCII), while git's messages are pinned
/// to English by `LC_MESSAGES` and `LANGUAGE`.
#[cfg(unix)]
#[test]
fn a_hook_sees_the_users_lang_and_no_lc_all() {
    let serial = common::env::env_serial();
    let dir = repo();
    let _env = common::env::bind_env(&serial)
        .set("LANG", "en_US.UTF-8")
        .remove("LC_ALL")
        .remove("LC_CTYPE");
    let seen = commit_and_record_hook_locale(dir.path());
    assert!(seen.contains("LC_ALL=<unset>\n"), "{seen}");
    assert!(seen.contains("LANG=en_US.UTF-8\n"), "{seen}");
    assert!(seen.contains("LC_CTYPE=<unset>\n"), "{seen}");
    assert!(seen.contains("LC_MESSAGES=C\n"), "{seen}");
    assert!(seen.contains("LANGUAGE=C\n"), "{seen}");
}

/// A parent `LC_ALL` with no `LANG`/`LC_CTYPE` is the user's only statement of
/// an encoding: it moves to `LC_CTYPE` (and `LC_ALL` goes) so the hook keeps it.
/// Messages stay English through `LC_MESSAGES`/`LANGUAGE`, which the hook sees.
#[cfg(unix)]
#[test]
fn an_lc_all_only_parent_reaches_the_hook_as_lc_ctype() {
    let serial = common::env::env_serial();
    let dir = repo();
    let _env = common::env::bind_env(&serial)
        .set("LC_ALL", "de_DE.UTF-8")
        .remove("LANG")
        .remove("LC_CTYPE");
    let seen = commit_and_record_hook_locale(dir.path());
    assert!(seen.contains("LC_ALL=<unset>\n"), "{seen}");
    assert!(seen.contains("LC_CTYPE=de_DE.UTF-8\n"), "{seen}");
    assert!(seen.contains("LC_MESSAGES=C\n"), "{seen}");
    assert!(seen.contains("LANGUAGE=C\n"), "{seen}");
}

/// [`repo`] with a configured identity, for the tests that commit through git
/// itself (merge, stash) or that read the identity back.
fn repo_with_identity() -> tempfile::TempDir {
    let dir = repo();
    run_git(dir.path(), &["config", "user.name", "Test User"]);
    run_git(dir.path(), &["config", "user.email", "test@example.com"]);
    dir
}

const BREW_GIT_DIR: &str = "/opt/homebrew/bin";

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
        // A check that could not run must not read as one that passed: CI sets
        // the variable where a translating git is installed, and a missing one
        // is then a failure.
        assert!(
            std::env::var("GITPULSE_REQUIRE_LOCALIZED_GIT").as_deref() != Ok("1"),
            "GITPULSE_REQUIRE_LOCALIZED_GIT=1 but no gettext-enabled git with a de catalog was found"
        );
        eprintln!("SKIPPED: no gettext-enabled git with a de/fr catalog found");
        return;
    };
    let dir = repo_with_identity();
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
    let dir = repo_with_identity();
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
        &[
            "config",
            "diff.x.textconv",
            &converter.display().to_string(),
        ],
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

    let shown =
        gitpulse_lib::engine::stash::show(dir.path().to_str().unwrap(), &oid).expect("stash show");
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
    let dir = repo_with_identity();
    let _env = common::env::bind_env(&serial)
        .remove("EMAIL")
        .remove("GIT_AUTHOR_NAME")
        .remove("GIT_AUTHOR_EMAIL")
        .remove("GIT_COMMITTER_NAME")
        .remove("GIT_COMMITTER_EMAIL");
    run_git(dir.path(), &["config", "author.name", "Author Cfg"]);
    run_git(
        dir.path(),
        &["config", "author.email", "author@example.com"],
    );
    trust_repo(dir.path());
    let (ok, line) = git_var_author(dir.path(), &[]);
    assert!(
        ok && line.starts_with("Author Cfg <author@example.com>"),
        "{line}"
    );
    let id = GitWriter::identity(dir.path().to_str().unwrap()).expect("identity");
    assert_eq!(
        id.name.as_deref(),
        Some("Author Cfg"),
        "git var said: {line}"
    );
    assert_eq!(
        id.email.as_deref(),
        Some("author@example.com"),
        "git var said: {line}"
    );
}

/// An empty `GIT_AUTHOR_NAME` makes `git var` (and `git commit`) refuse, even
/// with `user.name` set; `identity()` must not report a complete identity for a
/// state in which git would not record a commit.
#[test]
fn identity_is_incomplete_when_git_refuses_an_empty_author_name() {
    let serial = common::env::env_serial();
    let dir = repo_with_identity();
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
