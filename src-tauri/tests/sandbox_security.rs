//! Integration tests for sandbox hardening: symlink escapes through
//! `sandbox_write` and working-tree reads, plus the bounded `gh` CLI probe.

use gitpulse_lib::engine::git_cli::{git_text, sandbox_write};
use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::engine::git_writer::{
    validate_clone_url, validate_oid_or_revision, validate_ref_name, GitWriter,
};
use std::path::Path;
use std::time::{Duration, Instant};

/// Creates a plain (no commits needed) git repo so `validate_repo` accepts it.
fn init_repo(dir: &std::path::Path) {
    let output = std::process::Command::new("git")
        .arg("init")
        .current_dir(dir)
        .output()
        .expect("spawn git init");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// Writes `file`, stages it, and commits with a fixed identity.
fn commit_file(dir: &std::path::Path, file: &str, content: &str, msg: &str) {
    std::fs::write(dir.join(file), content).expect("write file");
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@t"])
        .args(["add", "--", file])
        .current_dir(dir)
        .output()
        .expect("spawn git add");
    assert!(output.status.success());
    let output = std::process::Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-m",
            msg,
        ])
        .current_dir(dir)
        .output()
        .expect("spawn git commit");
    assert!(
        output.status.success(),
        "commit failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// `git status --porcelain` output for assertions about index/tree state.
fn porcelain(dir: &std::path::Path) -> String {
    let output = std::process::Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(dir)
        .output()
        .expect("spawn git status");
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[cfg(unix)]
#[test]
fn sandbox_write_refuses_symlink_escape() {
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("target.txt"), "original").unwrap();

    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();
    let repo = repo_dir.path().canonicalize().unwrap();

    // Case 1: symlink directly to an existing file outside the repo. Pre-fix,
    // fs::write followed the link and overwrote the external file.
    std::os::unix::fs::symlink(outside.path().join("target.txt"), repo.join("file-leak")).unwrap();
    let err = sandbox_write(&repo_str, "file-leak", "pwned").expect_err("file symlink");
    assert!(err.contains("escapes the repository"), "got: {err}");
    assert_eq!(
        std::fs::read_to_string(outside.path().join("target.txt")).unwrap(),
        "original",
        "outside file must be untouched"
    );

    // Case 2: symlink to a directory outside the repo; the write lands in a
    // NEW nested path through the link.
    std::os::unix::fs::symlink(outside.path(), repo.join("dir-leak")).unwrap();
    let err =
        sandbox_write(&repo_str, "dir-leak/payload.txt", "pwned").expect_err("directory symlink");
    assert!(err.contains("escapes the repository"), "got: {err}");
    assert!(
        !outside.path().join("payload.txt").exists(),
        "nothing may be written through the directory link"
    );

    // Case 3: dangling symlink pointing at a non-existent outside target.
    // The link itself exists, so it must still be refused.
    std::os::unix::fs::symlink(outside.path().join("missing.txt"), repo.join("dangling")).unwrap();
    assert!(sandbox_write(&repo_str, "dangling", "pwned").is_err());
}

#[cfg(unix)]
#[test]
fn sandbox_write_still_creates_nested_paths_inside_repo() {
    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();

    sandbox_write(&repo_str, "src/deep/nested/new.txt", "hello").expect("nested write");
    assert_eq!(
        std::fs::read_to_string(repo_dir.path().join("src/deep/nested/new.txt")).unwrap(),
        "hello"
    );

    // Reading back through the reader works on the canonicalized path too.
    let content = GitReader::get_file_content(&repo_str, "src/deep/nested/new.txt", None)
        .expect("read back written file");
    assert_eq!(content, "hello");

    // An internal symlink (target inside the repo) remains readable — the fix
    // must not over-reject legitimate repos that use relative internal links.
    #[cfg(unix)]
    {
        let repo = repo_dir.path().canonicalize().unwrap();
        std::os::unix::fs::symlink(repo.join("src/deep/nested/new.txt"), repo.join("alias.txt"))
            .unwrap();
        let blob = GitReader::get_file_blob(&repo_str, "alias.txt", None).expect("internal link");
        assert_eq!(blob.text.as_deref(), Some("hello"));
    }
}

#[cfg(unix)]
#[test]
fn working_tree_reads_refuse_symlink_escape() {
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("secret.txt"), "top secret").unwrap();

    let repo_dir = tempfile::TempDir::new().unwrap();
    init_repo(repo_dir.path());
    let repo_str = repo_dir.path().to_string_lossy().into_owned();
    let repo = repo_dir.path().canonicalize().unwrap();
    std::os::unix::fs::symlink(outside.path().join("secret.txt"), repo.join("leak.txt")).unwrap();

    // Pre-fix both calls happily fs::read the outside file.
    assert!(GitReader::get_file_content(&repo_str, "leak.txt", None).is_err());
    assert!(GitReader::get_file_blob(&repo_str, "leak.txt", None).is_err());

    // Language stats: the untracked link is listed by `ls-files --others`.
    // Pre-fix the loop read through it and accrued LOC for Python from bytes
    // living outside the repo; post-fix the read is refused and the file
    // records zero code lines.
    let outside_py = tempfile::TempDir::new().unwrap();
    std::fs::write(outside_py.path().join("payload.py"), "print('pwned')\n").unwrap();
    std::os::unix::fs::symlink(outside_py.path().join("payload.py"), repo.join("evil.py")).unwrap();
    let stats = GitReader::get_repo_language_stats(&repo_str)
        .expect("language stats")
        .stats;
    assert!(
        stats
            .iter()
            .filter(|s| s.language == "Python")
            .all(|s| s.code_lines == 0),
        "no language may accrue lines from an escaped read: {stats:?}"
    );
}

/// A hung `gh` earlier on PATH must not block the presence probe forever:
/// pre-fix this test hung on `Command::output()`; post-fix it returns within
/// the probe timeout and reports absence.
#[cfg(unix)]
#[test]
fn hung_gh_probe_is_bounded() {
    let fake_bin = tempfile::TempDir::new().unwrap();
    let fake_gh = fake_bin.path().join("gh");
    std::fs::write(&fake_gh, "#!/bin/sh\nsleep 300\n").unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755)).unwrap();

    let old_path = std::env::var("PATH").unwrap_or_default();
    std::env::set_var(
        "PATH",
        format!("{}:{}", fake_bin.path().display(), old_path),
    );
    let start = Instant::now();
    let present = gitpulse_lib::github::gh_cli_present();
    std::env::set_var("PATH", old_path);

    assert!(!present, "a hanging gh must not count as present");
    assert!(
        start.elapsed() < Duration::from_secs(30),
        "probe took {:?}; it must be bounded well below the fake gh's 300s sleep",
        start.elapsed()
    );
}

// ---------------------------------------------------------------------------
// M4: GIT_CONFIG_PARAMETERS env injection gap
// ---------------------------------------------------------------------------

/// End-to-end proof that a planted `GIT_CONFIG_PARAMETERS` value cannot reach
/// git spawned through the engine. The payload sets `user.name`, which
/// outranks even repo-local config when it survives — so the repo-local name
/// must win exactly when the scrub works. The raw-git control step proves the
/// payload is actually potent (the test would be vacuous otherwise).
#[cfg(unix)]
#[test]
fn git_cli_strips_planted_git_config_parameters() {
    let dir = tempfile::TempDir::new().unwrap();
    init_repo(dir.path());
    commit_file(dir.path(), "tracked.txt", "base\n", "init");

    let output = std::process::Command::new("git")
        .args(["config", "--local", "user.name", "LOCAL_NAME"])
        .current_dir(dir.path())
        .output()
        .expect("spawn git config");
    assert!(output.status.success());

    // Process-global mutation is tolerable here: every other test that
    // reaches git through the engine strips exactly this variable (that is
    // the behavior under test), so parallel tests cannot observe the plant.
    std::env::set_var("GIT_CONFIG_PARAMETERS", "'user.name=PWNED_NAME'");

    let control = std::process::Command::new("git")
        .args(["config", "user.name"])
        .current_dir(dir.path())
        .output()
        .expect("spawn control git");
    let control_name = String::from_utf8_lossy(&control.stdout).trim().to_string();

    let ours = git_text(dir.path(), &["config", "user.name"])
        .expect("engine git call must succeed with planted env");
    let ours_name = ours.trim().to_string();

    std::env::remove_var("GIT_CONFIG_PARAMETERS");

    assert_eq!(
        control_name, "PWNED_NAME",
        "control failed: without the env var taking effect this test proves nothing"
    );
    assert_eq!(
        ours_name, "LOCAL_NAME",
        "planted GIT_CONFIG_PARAMETERS must not survive into the spawned env"
    );
}

// ---------------------------------------------------------------------------
// m5-writer-side: pathspec glob blast radius
// ---------------------------------------------------------------------------

/// Pre-fix, `discard_changes(repo, "*")` glob-expanded the pathspec across the
/// whole tree and wiped every untracked file while reporting success. With
/// `:(literal)` the path matches only a file literally named `*` (none here),
/// so nothing may be deleted and the no-match outcome must surface as an error.
#[test]
fn discard_changes_star_is_literal_and_wipes_nothing() {
    let dir = tempfile::TempDir::new().unwrap();
    init_repo(dir.path());
    commit_file(dir.path(), "tracked.txt", "base\n", "init");
    std::fs::write(dir.path().join("alpha.txt"), "keep alpha\n").unwrap();
    std::fs::write(dir.path().join("beta.txt"), "keep beta\n").unwrap();

    let result = GitWriter::discard_changes(dir.path().to_str().unwrap(), "*");
    assert!(
        result.is_err(),
        "a literal '*' matching nothing must error, got {result:?}"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("alpha.txt")).unwrap(),
        "keep alpha\n",
        "alpha.txt must survive a '*' discard"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("beta.txt")).unwrap(),
        "keep beta\n",
        "beta.txt must survive a '*' discard"
    );
    assert_eq!(
        std::fs::read_to_string(dir.path().join("tracked.txt")).unwrap(),
        "base\n"
    );
}

/// A filename containing glob metacharacters must stage/unstage as itself:
/// pre-fix `git add -- 'lit[eral]?.txt'` interpreted the brackets and matched
/// nothing, failing outright.
#[test]
fn stage_and_unstage_treat_glob_paths_literally() {
    let dir = tempfile::TempDir::new().unwrap();
    init_repo(dir.path());
    commit_file(dir.path(), "tracked.txt", "base\n", "init");
    let weird = "lit[eral]?.txt";
    std::fs::write(dir.path().join(weird), "weird name\n").unwrap();
    let repo_str = dir.path().to_str().unwrap();

    GitWriter::stage_file(repo_str, weird).expect("literal staging of bracketed name");
    let staged = porcelain(dir.path());
    assert!(
        staged.contains(weird) && staged.starts_with('A'),
        "file must land in the index literally, got porcelain: {staged:?}"
    );

    GitWriter::unstage_file(repo_str, weird).expect("literal unstaging");
    let unstaged = porcelain(dir.path());
    assert!(
        unstaged.contains("??") && unstaged.contains(weird),
        "file must be back to untracked, got porcelain: {unstaged:?}"
    );
}

// ---------------------------------------------------------------------------
// m12: clone URL transport guard
// ---------------------------------------------------------------------------

/// Pseudo-transports (`ext::`, `fd::`, any `<scheme>::`) hand their argument
/// string to an arbitrary helper and can execute local commands; option-shaped
/// URLs are refused too. Allowlisted transports and local paths pass.
#[test]
fn clone_urls_with_command_transports_are_rejected() {
    for good in [
        "https://github.com/acme/gitpulse.git",
        "ssh://git@host/team/repo.git",
        "git://host/repo.git",
        "ftp://host/repo.git",
        "ftps://host/repo.git",
        "http://example.com/repo.git",
        "file:///tmp/some/repo.git",
        "/tmp/local/path",
        "some-relative-name",
        "git@github.com:acme/gitpulse.git",
    ] {
        assert!(validate_clone_url(good).is_ok(), "{good} must be allowed");
    }
    for evil in [
        "ext::sh -c touch /tmp/gitpulse-pwned",
        "fd::9",
        "vsock::1234",
        "weird-scheme::data",
        "-oProxyCommand=evil",
        "",
        "has\0nul",
    ] {
        assert!(
            validate_clone_url(evil).is_err(),
            "{evil:?} must be rejected"
        );
    }
}

/// The guard runs inside `clone_repo` itself, before anything spawns: an
/// `ext::` URL must be refused and its payload must never execute.
#[test]
fn clone_repo_refuses_ext_transport_without_executing_payload() {
    let marker = tempfile::TempDir::new().unwrap();
    let dest = tempfile::TempDir::new().unwrap();
    let target = dest.path().join("evil-clone");
    let evil_url = format!("ext::sh -c touch {}", marker.path().join("pwned").display());

    let result = GitWriter::clone_repo(&evil_url, target.to_str().unwrap());
    assert!(result.is_err(), "ext:: transport must be rejected");
    assert!(
        !marker.path().join("pwned").exists(),
        "the ext:: payload must never have executed"
    );
    assert!(!target.exists(), "no partial clone target may remain");
}

// ---------------------------------------------------------------------------
// finding 9: refname validator tightening
// ---------------------------------------------------------------------------

#[test]
fn ref_names_reject_lock_suffix_on_any_component() {
    assert!(validate_ref_name("feature.lock").is_err());
    assert!(validate_ref_name("feature/foo.lock").is_err());
    assert!(validate_ref_name("foo.lock/bar").is_err());
    // Non-suffix occurrences stay legal.
    assert!(validate_ref_name("lock").is_ok());
    assert!(validate_ref_name("foo.lockdown").is_ok());
    assert!(validate_ref_name("feature/lockmaker/x").is_ok());
}

/// Tightened revision grammar: ranges and reflog/peel/`rev:path` syntax are
/// refused because every caller passes a single commit-ish only (see the
/// caller audit on `validate_oid_or_revision`). Ordinary revisions keep working.
#[test]
fn revision_specs_reject_ranges_and_spec_syntax() {
    for ok in ["main", "HEAD~3", "HEAD^", "a1b2c3d4"] {
        assert!(validate_oid_or_revision(ok).is_ok(), "{ok} must stay valid");
    }
    for bad in [
        "a..b",
        "@{u}",
        "HEAD@{1}",
        "HEAD^{tree}",
        "rev:path.txt",
        "; rm -rf /",
        "-evil",
    ] {
        assert!(
            validate_oid_or_revision(bad).is_err(),
            "{bad:?} must be rejected"
        );
    }
}

// ---------------------------------------------------------------------------
// m9: clone timeout/failure leaves <dest>/.git
// ---------------------------------------------------------------------------

/// A source whose tree object is corrupt makes the clone fail late — after
/// git has materialized `<dest>/.git` — which used to wedge every retry into
/// "Already cloned". The writer must remove the skeleton it created and say
/// so, leaving the destination clean for a retry.
#[test]
fn failed_clone_removes_partial_git_dir_and_reports_cleanup() {
    let src = tempfile::TempDir::new().unwrap();
    init_repo(src.path());
    commit_file(src.path(), "tracked.txt", "base\n", "init");

    // Corrupt the root tree object so the transfer succeeds but checkout dies.
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD^{tree}"])
        .current_dir(src.path())
        .output()
        .expect("rev-parse HEAD^{{tree}}");
    assert!(output.status.success());
    let oid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    assert_eq!(oid.len(), 40, "unexpected rev-parse output: {oid:?}");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let obj = src
            .path()
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::set_permissions(&obj, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::fs::write(&obj, b"garbage").unwrap();
    }
    #[cfg(windows)]
    {
        let obj = src
            .path()
            .join(".git/objects")
            .join(&oid[..2])
            .join(&oid[2..]);
        std::fs::write(&obj, b"garbage").unwrap();
    }

    let dest = tempfile::TempDir::new().unwrap();
    // Non-existent final component so the clone lands exactly at `target`.
    // A plain local path is an allowlisted transport AND the one that fails
    // LATE (checkout stage): over file:// git dies during pack transfer
    // before .git is materialized, which would make this test vacuous.
    let target = dest.path().join("clone-target");
    let src_uri = src.path().to_string_lossy().into_owned();

    let first = GitWriter::clone_repo(&src_uri, target.to_str().unwrap());
    assert!(first.is_err(), "corrupt-source clone must fail");

    assert!(
        !target.join(".git").exists(),
        "partial '.git' left behind by the failed clone must be removed"
    );

    // Retryability: the second attempt must fail with the clone error again,
    // never with "Already cloned" (which would prove leftovers survived).
    let second = GitWriter::clone_repo(&src_uri, target.to_str().unwrap())
        .expect_err("retry must still hit the corrupt source");
    assert!(
        !second.contains("Already cloned"),
        "cleanup must keep retries working, got: {second}"
    );
    if let Err(err) = first {
        assert!(!err.trim().is_empty(), "failure must carry a diagnosis");
    }
}

/// Sanity: a healthy local clone via the allowlisted file:// transport still
/// works end to end after the guard landed.
#[test]
fn clone_repo_still_clones_local_file_urls() {
    let src = tempfile::TempDir::new().unwrap();
    init_repo(src.path());
    commit_file(src.path(), "tracked.txt", "base\n", "init");

    let dest = tempfile::TempDir::new().unwrap();
    let target = dest.path().join("healthy");
    let cloned = GitWriter::clone_repo(
        &format!("file://{}", src.path().display()),
        target.to_str().unwrap(),
    )
    .expect("healthy local clone must succeed");
    assert!(Path::new(&cloned).join(".git").exists());
    assert_eq!(
        std::fs::read_to_string(Path::new(&cloned).join("tracked.txt")).unwrap(),
        "base\n"
    );
}
