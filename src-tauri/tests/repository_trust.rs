//! Repository-defined commands require explicit trust before automatic reads.

#![cfg(unix)]

use gitpulse_lib::engine::git_reader::GitReader;
use gitpulse_lib::{engine::git_cli, repository_trust};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("spawn fixture git");
    assert!(
        output.status.success(),
        "fixture git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    git(dir.path(), &["init", "-q", "-b", "main"]);
    dir
}

fn approve(repo: &Path) {
    let path = repo.to_str().unwrap();
    let view = repository_trust::inspect(path).unwrap();
    repository_trust::grant(path, &view.identity, false).unwrap();
}

#[test]
fn global_probes_do_not_inherit_repository_configuration() {
    let dir = fixture();
    let nested = dir.path().join("nested");
    std::fs::create_dir(&nested).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "global_probe_subprocess", "--ignored"])
        .current_dir(&nested)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stdout)
    );
}

#[test]
#[ignore = "fixture helper invoked explicitly with a repository working directory"]
fn global_probe_subprocess() {
    let inherited = std::env::current_dir().unwrap();
    assert!(git_cli::find_git_root(&inherited).is_some());
    let output =
        git_cli::capture_command("pwd", &[], None, std::time::Duration::from_secs(2), &[]).unwrap();
    assert!(output.success);
    let actual = String::from_utf8_lossy(&output.stdout);
    assert_eq!(
        actual.trim(),
        "/",
        "global probes must have an explicit neutral working directory"
    );
    assert!(git_cli::capture_command(
        "pwd",
        &[],
        Some(&inherited),
        std::time::Duration::from_secs(2),
        &[]
    )
    .unwrap_err()
    .contains(repository_trust::REQUIRED));
    let source = inherited.join("relative-source");
    std::fs::create_dir(&source).unwrap();
    git(&source, &["init", "-q"]);
    let destination = inherited.join("relative-clone");
    gitpulse_lib::engine::GitWriter::clone_repo("relative-source", destination.to_str().unwrap())
        .expect("relative local clone must retain the caller's path interpretation");
    assert!(destination.join(".git").is_dir());
}

#[test]
fn direct_git_and_captured_tools_cannot_bypass_admission() {
    let dir = fixture();
    let repo = dir.path();
    assert!(git_cli::git(repo, &["status"])
        .unwrap_err()
        .contains(repository_trust::REQUIRED));
    assert!(git_cli::git_text_partial(repo, &["status"]).is_err());
    assert!(git_cli::git_text_capped(repo, &["status"], 100).is_err());
    assert!(git_cli::git_with_stdin(repo, &["status"], b"").is_err());
    let subdir = repo.join("nested");
    std::fs::create_dir(&subdir).unwrap();
    let run = git_cli::capture_command(
        "sh",
        &["-c", "touch sentinel"],
        Some(&subdir),
        std::time::Duration::from_secs(2),
        &[],
    );
    assert!(run.unwrap_err().contains(repository_trust::REQUIRED));
    assert!(!subdir.join("sentinel").exists());
    approve(repo);
    assert!(git_cli::git(repo, &["status", "--porcelain"]).is_ok());
    assert!(
        git_cli::capture_command(
            "sh",
            &["-c", "touch sentinel"],
            Some(&subdir),
            std::time::Duration::from_secs(2),
            &[]
        )
        .unwrap()
        .success
    );
    assert!(subdir.join("sentinel").exists());
}

#[test]
fn symlink_aliases_match_but_replacements_and_stale_approvals_do_not() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let alias = dir.path().join("alias");
    std::os::unix::fs::symlink(&repo, &alias).unwrap();
    let preview = repository_trust::inspect(repo.to_str().unwrap()).unwrap();
    approve(&repo);
    repository_trust::require(&alias).unwrap();
    std::fs::rename(&repo, dir.path().join("old")).unwrap();
    std::fs::create_dir(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    assert!(repository_trust::require(&repo).is_err());
    assert!(
        repository_trust::grant(repo.to_str().unwrap(), &preview.identity, false)
            .unwrap_err()
            .contains("changed")
    );
}

#[test]
fn linked_worktrees_and_bare_repositories_need_their_own_approval() {
    let repo = fixture();
    approve(repo.path());
    git(
        repo.path(),
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked");
    git(
        repo.path(),
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
    assert!(repository_trust::require(&linked).is_err());
    approve(&linked);
    git_cli::git(&linked, &["status"]).unwrap();
    let other = fixture();
    std::fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", other.path().join(".git").display()),
    )
    .unwrap();
    assert!(repository_trust::require(&linked).is_err());
    let bare = tempfile::tempdir().unwrap();
    git(bare.path(), &["init", "--bare", "-q"]);
    assert!(git_cli::resolve_repo(bare.path().to_str().unwrap()).is_err());
    approve(bare.path());
    assert!(
        git_cli::resolve_repo(bare.path().to_str().unwrap())
            .unwrap()
            .is_bare
    );
}

#[test]
fn included_configuration_is_denied_before_git_reads_it() {
    let repo = fixture();
    std::fs::write(repo.path().join("file"), "data\n").unwrap();
    git(repo.path(), &["add", "file"]);
    let hook = repo.path().join(".git/hook");
    std::fs::write(&hook, "#!/bin/sh\ntouch included-hook\nprintf 'token\\0'\n").unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    std::fs::write(
        repo.path().join(".git/included"),
        format!("[core]\n fsmonitor = {}\n", hook.display()),
    )
    .unwrap();
    git(repo.path(), &["config", "include.path", "included"]);
    assert!(GitReader::get_status(repo.path().to_str().unwrap()).is_err());
    assert!(!repo.path().join("included-hook").exists());
    approve(repo.path());
    GitReader::get_status(repo.path().to_str().unwrap()).unwrap();
    assert!(repo.path().join("included-hook").exists());
}

#[test]
fn health_watchers_pty_and_mcp_are_denied_before_work_starts() {
    let repo = fixture();
    let path = repo.path().to_str().unwrap();
    std::fs::create_dir(repo.path().join(".cargo")).unwrap();
    std::fs::write(repo.path().join("Cargo.lock"), "version = 4\n").unwrap();
    std::fs::write(
        repo.path().join(".cargo/config.toml"),
        "[alias]\naudit = \"!touch cargo-alias-executed\"\n",
    )
    .unwrap();
    let health = gitpulse_lib::analyzer::DepsScanner::scan(path);
    assert!(health.unwrap_err().contains(repository_trust::REQUIRED));
    assert!(!repo.path().join("cargo-alias-executed").exists());
    assert!(gitpulse_lib::devmap::cli::build(path)
        .unwrap_err()
        .contains(repository_trust::REQUIRED));
    let changes = gitpulse_lib::insights::active_changes(path, None, None);
    assert!(!changes.ok);
    assert!(changes.error.contains(repository_trust::REQUIRED));
    let app = tauri::test::mock_builder()
        .build(gitpulse_lib::context())
        .unwrap();
    let terminals = gitpulse_lib::terminal::TerminalSessions::default();
    let spawn = gitpulse_lib::terminal::spawn_session(
        app.handle(),
        &terminals,
        path,
        24,
        80,
        Some("/bin/sh".into()),
        Some(vec!["-c".into(), "touch terminal-executed".into()]),
        None,
    );
    assert!(spawn.unwrap_err().contains(repository_trust::REQUIRED));
    assert!(!repo.path().join("terminal-executed").exists());
}

#[test]
fn metadata_inputs_are_bounded_and_untrusted_repository_records_have_no_authority() {
    let repo = fixture();
    let path = repo.path().to_str().unwrap();
    let preview = repository_trust::inspect(path).unwrap();
    std::fs::create_dir(repo.path().join(".devcouncil")).unwrap();
    std::fs::write(
        repo.path().join(".devcouncil/repository-trust.json"),
        &preview.identity,
    )
    .unwrap();
    assert!(repository_trust::require(repo.path()).is_err());
    let link = tempfile::tempdir().unwrap();
    for value in [
        "gitdir: \n".to_owned(),
        "gitdir: /tmp/no\nother".to_owned(),
        format!("gitdir: {}", "x".repeat(17000)),
    ] {
        std::fs::write(link.path().join(".git"), value).unwrap();
        assert!(repository_trust::inspect(link.path().to_str().unwrap()).is_err());
    }
}

#[test]
fn concurrent_grant_revoke_cycles_never_authorize_other_repositories() {
    let blocked = fixture();
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let blocked = blocked.path();
            scope.spawn(move || {
                let own = fixture();
                let path = own.path().to_str().unwrap();
                for _ in 0..64 {
                    assert!(repository_trust::require(own.path()).is_err());
                    approve(own.path());
                    repository_trust::require(own.path()).unwrap();
                    assert!(repository_trust::require(blocked).is_err());
                    repository_trust::revoke(path).unwrap();
                    assert!(repository_trust::require(own.path()).is_err());
                }
            });
        }
    });
}

fn linked_fixture(main: &Path, linked: &Path) {
    git(
        main,
        &[
            "-c",
            "user.name=test",
            "-c",
            "user.email=test@example.com",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ],
    );
    git(
        main,
        &["worktree", "add", "--detach", linked.to_str().unwrap()],
    );
}

#[test]
fn worktree_removal_cannot_execute_an_unapproved_targets_configuration() {
    let main = fixture();
    approve(main.path());
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked");
    linked_fixture(main.path(), &linked);
    git(
        main.path(),
        &["config", "extensions.worktreeConfig", "true"],
    );
    let marker = parent.path().join("remove-helper-executed");
    let hook = parent.path().join("probe.sh");
    std::fs::write(
        &hook,
        format!("#!/bin/sh\n: > '{}'\nprintf 'token\\0'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700)).unwrap();
    git(
        &linked,
        &[
            "config",
            "--worktree",
            "core.fsmonitor",
            hook.to_str().unwrap(),
        ],
    );
    let result = gitpulse_lib::engine::worktree::remove_worktree(
        main.path().to_str().unwrap(),
        linked.to_str().unwrap(),
        false,
    );
    assert!(
        !marker.exists(),
        "removing an unapproved worktree executed its helper: {result:?}"
    );
    assert!(result.unwrap_err().contains(repository_trust::REQUIRED));
    assert!(linked.exists());
    approve(&linked);
    gitpulse_lib::engine::worktree::remove_worktree(
        main.path().to_str().unwrap(),
        linked.to_str().unwrap(),
        false,
    )
    .unwrap();
    assert!(!linked.exists());
}

#[test]
fn trusting_a_linked_checkout_preserves_shared_ledger_without_authorizing_main() {
    let main = fixture();
    let parent = tempfile::tempdir().unwrap();
    let linked = parent.path().join("linked");
    linked_fixture(main.path(), &linked);
    approve(&linked);
    let address = gitpulse_lib::ledger::bindings::repository_address(linked.to_str().unwrap());
    assert!(
        address.is_ok(),
        "approved linked checkout lost its ledger: {address:?}"
    );
    assert!(repository_trust::require(main.path()).is_err());
}

#[test]
fn status_read_does_not_execute_an_untrusted_repository_fsmonitor() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repo = fixture.path().join("repo");
    std::fs::create_dir(&repo).expect("repo directory");
    git(&repo, &["init", "-q"]);
    std::fs::write(repo.join("tracked.txt"), "fixture\n").expect("fixture file");
    git(&repo, &["add", "--", "tracked.txt"]);

    let hook = repo.join(".git/fsmonitor-probe.sh");
    std::fs::write(
        &hook,
        "#!/bin/sh\n: > fsmonitor-executed\nprintf 'probe-token\\0'\n",
    )
    .expect("harmless hook");
    std::fs::set_permissions(&hook, std::fs::Permissions::from_mode(0o700))
        .expect("executable hook");
    git(
        &repo,
        &[
            "config",
            "core.fsmonitor",
            hook.to_str().expect("hook path"),
        ],
    );

    let result = GitReader::get_status(repo.to_str().expect("repo path"));

    assert!(
        !repo.join("fsmonitor-executed").exists(),
        "an automatic status read executed repository-defined code before trust: {result:?}"
    );
    assert!(result.unwrap_err().contains(repository_trust::REQUIRED));
    let preview = repository_trust::inspect(repo.to_str().unwrap()).unwrap();
    assert!(!preview.trusted);
    assert!(!repo.join("fsmonitor-executed").exists());
    repository_trust::grant(repo.to_str().unwrap(), &preview.identity, false).unwrap();
    let status = GitReader::get_status(repo.to_str().unwrap()).unwrap();
    assert!(status.iter().any(|file| file.path == "tracked.txt"));
    assert!(
        repo.join("fsmonitor-executed").exists(),
        "trusted helpers remain supported"
    );
    repository_trust::revoke(repo.to_str().unwrap()).unwrap();
    std::fs::remove_file(repo.join("fsmonitor-executed")).unwrap();
    assert!(GitReader::get_status(repo.to_str().unwrap()).is_err());
    assert!(!repo.join("fsmonitor-executed").exists());
}
