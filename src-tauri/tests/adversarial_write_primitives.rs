//! Adversarial review of ft-f16fad35 (editor/explorer write primitives): each
//! test attacks the `.git` refusal, exclusive create, save and move/delete
//! paths through the same entry points the IPC commands call.

#![cfg(unix)]

use gitpulse_lib::engine::git_cli::{
    sandbox_create, sandbox_save_edited_text, sandbox_write, FileWriteError,
};
use gitpulse_lib::engine::git_writer::GitWriter;
use std::path::{Path, PathBuf};

mod common;

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    repo_str: String,
}

fn fixture() -> Fixture {
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    common::run_git(&repo, &["init"]);
    common::run_git(&repo, &["config", "commit.gpgsign", "false"]);
    let repo_str = repo.to_string_lossy().into_owned();
    Fixture {
        _dir: dir,
        repo,
        repo_str,
    }
}

fn commit_all(repo: &Path) {
    common::run_git(repo, &["add", "-A"]);
    common::run_git(repo, &["commit", "-q", "-m", "c"]);
}

fn config(repo: &Path) -> Vec<u8> {
    std::fs::read(repo.join(".git/config")).unwrap()
}

/// ATTACK 1: spellings of `.git` that the lexical check might miss. Every
/// write entry point must refuse them and leave `.git/config` untouched.
#[test]
fn dot_git_spellings_are_refused_by_every_write_entry_point() {
    let f = fixture();
    std::fs::write(f.repo.join("src.txt"), "x").unwrap();
    let before = config(&f.repo);
    let spellings = [
        ".Git/config",
        ".gIT/config",
        "./.git/config",
        "sub/../.git/config",
        ".git/",
        ".git/./config",
        ".git//config",
        ".git. ./config",
        ".git::$INDEX_ALLOCATION/config",
        ".g\u{200c}it/config",
        ".g\u{200d}it/config",
        ".\u{feff}git/config",
        "\u{200c}.git/config",
        ".git\u{200b}/config",
        ".gi\u{0307}t/config",
    ];
    let mut leaks = Vec::new();
    for spelling in spellings {
        // Only a spelling the filesystem resolves to the real `.git` is an
        // attack; one it treats as a different name is just a odd directory.
        let resolves = std::fs::metadata(f.repo.join(spelling.trim_end_matches('/')))
            .is_ok_and(|m| m.is_file() || m.is_dir());
        let create = sandbox_create(&f.repo_str, &format!("{spelling}x"));
        let save = sandbox_save_edited_text(&f.repo_str, spelling, "[core]\n", None);
        let raw = sandbox_write(&f.repo_str, spelling, "[core]\n");
        let moved = GitWriter::move_path(&f.repo_str, "src.txt", &format!("{spelling}x"), |_| Ok(()));
        if resolves && (create.is_ok() || save.is_ok() || raw.is_ok() || moved.is_ok()) {
            leaks.push(format!(
                "{spelling:?} resolves to .git but create={create:?} save={save:?} raw={raw:?} move={moved:?}"
            ));
        }
        if moved.is_ok() {
            let _ = std::fs::rename(f.repo.join(format!("{spelling}x")), f.repo.join("src.txt"));
        }
    }
    assert_eq!(config(&f.repo), before, ".git/config changed");
    assert!(leaks.is_empty(), "{leaks:#?}");
}

/// ATTACK 1: a symlink to `.git` inside the tree (alias) cannot be created
/// through, moved into, or deleted.
#[test]
fn a_symlink_alias_of_dot_git_cannot_be_moved_into_or_deleted_through() {
    let f = fixture();
    std::os::unix::fs::symlink(f.repo.join(".git"), f.repo.join("meta")).unwrap();
    std::fs::write(f.repo.join("x"), "x").unwrap();
    assert!(sandbox_create(&f.repo_str, "meta/hooks/pre-commit").is_err());
    assert!(GitWriter::move_path(&f.repo_str, "x", "meta/hooks/x", |_| Ok(())).is_err());
    assert!(GitWriter::delete_path(&f.repo_str, "meta/config", |_| Ok(())).is_err());
    assert!(f.repo.join(".git/config").exists());
}

/// ATTACK 1: moving a symlink must move the link, never the file it names.
#[test]
fn moving_a_symlink_moves_the_link_not_its_target() {
    let f = fixture();
    std::fs::write(f.repo.join("real.txt"), "payload").unwrap();
    std::os::unix::fs::symlink("real.txt", f.repo.join("link")).unwrap();
    let result = GitWriter::move_path(&f.repo_str, "link", "link2", |_| Ok(()));
    assert!(result.is_ok(), "{result:?}");
    assert!(
        f.repo.join("real.txt").exists(),
        "move_path renamed the link's TARGET real.txt instead of the link"
    );
    assert!(std::fs::symlink_metadata(f.repo.join("link2")).unwrap().is_symlink());
}

/// ATTACK 1: `./.` names the repository root; deleting it must be refused the
/// way `.` is, and must not remove tracked content.
#[test]
fn delete_path_refuses_every_spelling_of_the_repository_root() {
    let f = fixture();
    std::fs::write(f.repo.join("keep.txt"), "keep").unwrap();
    commit_all(&f.repo);
    for spelling in ["./.", "./", ".//", "sub/..", "a/b/../.."] {
        let result = GitWriter::delete_path(&f.repo_str, spelling, |_| Ok(()));
        assert!(result.is_err(), "{spelling:?} deleted: {result:?}");
        assert!(f.repo.join("keep.txt").exists(), "{spelling:?} removed keep.txt");
    }
}

/// ATTACK 1: the same root spelling as a move source.
#[test]
fn move_path_refuses_the_repository_root_as_a_source() {
    let f = fixture();
    std::fs::write(f.repo.join("keep.txt"), "keep").unwrap();
    commit_all(&f.repo);
    for spelling in [".", "./.", "./"] {
        let result = GitWriter::move_path(&f.repo_str, spelling, "moved", |_| Ok(()));
        assert!(result.is_err(), "{spelling:?} moved the root: {result:?}");
        assert!(f.repo.join("keep.txt").exists());
    }
}

/// ATTACK 1: a directory that contains a nested repository must not be
/// deleted out from under it, and its `.git` is never a path.
#[test]
fn deleting_a_directory_with_a_nested_repository_keeps_its_dot_git() {
    let f = fixture();
    let sub = f.repo.join("sub");
    std::fs::create_dir(&sub).unwrap();
    common::run_git(&sub, &["init"]);
    std::fs::write(sub.join("note.txt"), "n").unwrap();
    let result = GitWriter::delete_path(&f.repo_str, "sub", |_| Ok(()));
    assert!(sub.join(".git").exists(), "nested .git removed: {result:?}");
    assert!(GitWriter::delete_path(&f.repo_str, "sub/.git", |_| Ok(())).is_err());
    assert!(sandbox_create(&f.repo_str, "sub/.git/hooks/x").is_err());
}

/// ATTACK 3 (IPC level): a dangling symlink at the create path reports
/// `already_exists`, like a live one, and writes nothing through it.
#[test]
fn create_over_a_dangling_symlink_reports_already_exists() {
    let f = fixture();
    std::os::unix::fs::symlink("nowhere", f.repo.join("l")).unwrap();
    let result = sandbox_create(&f.repo_str, "l");
    assert!(
        matches!(result, Err(FileWriteError::AlreadyExists { .. })),
        "{result:?}"
    );
    assert!(!f.repo.join("nowhere").exists());
}

/// ATTACK 3: a dangling symlink whose target is outside the tree.
#[test]
fn create_over_a_dangling_symlink_to_outside_writes_nothing_outside() {
    let f = fixture();
    let outside = tempfile::TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path().join("new"), f.repo.join("l")).unwrap();
    let result = sandbox_create(&f.repo_str, "l");
    assert!(result.is_err());
    assert!(!outside.path().join("new").exists());
}

/// ATTACK 3: a parent that is a symlink out of the repository.
#[test]
fn create_under_a_symlinked_parent_outside_is_refused() {
    let f = fixture();
    let outside = tempfile::TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), f.repo.join("out")).unwrap();
    assert!(sandbox_create(&f.repo_str, "out/new.txt").is_err());
    assert!(sandbox_create(&f.repo_str, "out/deeper/new.txt").is_err());
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
}

/// ATTACK 3: a `.gitkeep` that is a symlink is "already exists" and is not
/// written through.
#[test]
fn a_gitkeep_symlink_is_not_written_through() {
    let f = fixture();
    std::fs::create_dir(f.repo.join("d")).unwrap();
    std::fs::write(f.repo.join("target.txt"), "keep").unwrap();
    std::os::unix::fs::symlink("../target.txt", f.repo.join("d/.gitkeep")).unwrap();
    let result = sandbox_create(&f.repo_str, "d/.gitkeep");
    assert!(
        matches!(result, Err(FileWriteError::AlreadyExists { .. })),
        "{result:?}"
    );
    assert_eq!(std::fs::read(f.repo.join("target.txt")).unwrap(), b"keep");
}

/// ATTACK 3: an over-long name is refused without a panic and without
/// stranding the empty parents the create made on the way.
#[test]
fn an_overlong_create_name_strands_no_directories() {
    let f = fixture();
    let long = "n".repeat(300);
    let result = sandbox_create(&f.repo_str, &format!("fresh/dir/{long}"));
    assert!(result.is_err());
    assert!(!f.repo.join("fresh").exists(), "left `fresh/dir` behind");
}

/// ATTACK 4: a save through a base whose file was renamed away while open is
/// `changed_on_disk` and does not recreate the old path.
#[test]
fn a_save_after_the_file_was_renamed_is_a_conflict_and_recreates_nothing() {
    let f = fixture();
    std::fs::write(f.repo.join("a.txt"), "one\n").unwrap();
    let base = gitpulse_lib::engine::git_reader::GitReader::get_file_blob(&f.repo_str, "a.txt", None)
        .unwrap()
        .disk_base
        .unwrap();
    std::fs::rename(f.repo.join("a.txt"), f.repo.join("b.txt")).unwrap();
    let result = sandbox_save_edited_text(&f.repo_str, "a.txt", "mine\n", Some(&base));
    assert!(matches!(result, Err(FileWriteError::ChangedOnDisk { .. })), "{result:?}");
    assert!(!f.repo.join("a.txt").exists());
}

/// ATTACK 4: the conflict's `actual` token, handed back as the base, saves
/// exactly once; a second external edit in between conflicts again (Overwrite
/// re-checks rather than writing blindly).
#[test]
fn overwrite_with_the_conflict_token_rechecks_the_disk() {
    let f = fixture();
    std::fs::write(f.repo.join("a.txt"), "one\n").unwrap();
    let base = gitpulse_lib::engine::git_reader::GitReader::get_file_blob(&f.repo_str, "a.txt", None)
        .unwrap()
        .disk_base
        .unwrap();
    std::fs::write(f.repo.join("a.txt"), "two\n").unwrap();
    let Err(FileWriteError::ChangedOnDisk { actual, .. }) =
        sandbox_save_edited_text(&f.repo_str, "a.txt", "mine\n", Some(&base))
    else {
        panic!("expected a conflict");
    };
    std::fs::write(f.repo.join("a.txt"), "three\n").unwrap();
    let again = sandbox_save_edited_text(&f.repo_str, "a.txt", "mine\n", Some(&actual));
    assert!(matches!(again, Err(FileWriteError::ChangedOnDisk { .. })), "{again:?}");
    assert_eq!(std::fs::read(f.repo.join("a.txt")).unwrap(), b"three\n");
}

/// ATTACK 4: an Overwrite (token of the symlink-swapped-in file) must not
/// write through a symlink swapped in at the path after the conflict.
#[test]
fn overwrite_does_not_write_through_a_symlink_swapped_in_after_the_conflict() {
    let f = fixture();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::write(outside.path().join("victim"), "outside").unwrap();
    std::fs::write(f.repo.join("a.txt"), "one\n").unwrap();
    std::fs::remove_file(f.repo.join("a.txt")).unwrap();
    std::os::unix::fs::symlink(outside.path().join("victim"), f.repo.join("a.txt")).unwrap();
    let result = sandbox_save_edited_text(&f.repo_str, "a.txt", "mine\n", None);
    assert!(result.is_err());
    assert_eq!(std::fs::read(outside.path().join("victim")).unwrap(), b"outside");
}

/// ATTACK 4: a save of an executable through the whole sandbox path keeps +x.
#[test]
fn saving_an_executable_keeps_plus_x_through_the_sandbox() {
    use std::os::unix::fs::PermissionsExt;
    let f = fixture();
    let p = f.repo.join("run.sh");
    std::fs::write(&p, "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    let base = gitpulse_lib::engine::git_reader::GitReader::get_file_blob(&f.repo_str, "run.sh", None)
        .unwrap()
        .disk_base;
    sandbox_save_edited_text(&f.repo_str, "run.sh", "#!/bin/sh\necho\n", base.as_deref()).unwrap();
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o111, 0o111);
}

/// ATTACK 1: a linked worktree's private git dir (`<common>/worktrees/x`) is
/// protected from the worktree's own saves, creates and moves, whatever name
/// reaches it.
#[test]
fn a_linked_worktree_cannot_write_its_gitdir_or_the_common_dir() {
    let f = fixture();
    std::fs::write(f.repo.join("a.txt"), "x").unwrap();
    commit_all(&f.repo);
    let wt_dir = tempfile::TempDir::new().unwrap();
    let wt = wt_dir.path().canonicalize().unwrap().join("wt");
    common::run_git(&f.repo, &["worktree", "add", "-q", wt.to_str().unwrap(), "-b", "w"]);
    common::trust_repo(&wt);
    let wt_str = wt.to_string_lossy().into_owned();
    let before = config(&f.repo);
    for path in [".git", ".git/config", ".git/hooks/x"] {
        assert!(sandbox_create(&wt_str, path).is_err(), "{path}");
        assert!(sandbox_save_edited_text(&wt_str, path, "x", None).is_err(), "{path}");
        assert!(GitWriter::move_path(&wt_str, "a.txt", path, |_| Ok(())).is_err(), "{path}");
    }
    // The `.git` FILE itself is not savable either.
    let gitfile = std::fs::read(wt.join(".git")).unwrap();
    assert!(sandbox_save_edited_text(&wt_str, ".git", "gitdir: /tmp/x\n", None).is_err());
    assert_eq!(std::fs::read(wt.join(".git")).unwrap(), gitfile);
    assert_eq!(config(&f.repo), before);
}
