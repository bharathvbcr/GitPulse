//! Adversarial review of ft-8f398bb6 (nested gitdir guard, dotgit spellings,
//! byte-exact Overwrite). Tests marked `#[ignore = "break: ..."]` assert the
//! behaviour the guard should have and currently does not; run them with
//! `--ignored` to see the break. The rest pin behaviour that holds.

#![cfg(unix)]

use gitpulse_lib::engine::git_cli::{
    is_git_metadata_component, sandbox_create, sandbox_join_write, sandbox_join_write_entry,
    sandbox_overwrite_edited_text, sandbox_write, FileWriteError,
};
use gitpulse_lib::engine::git_writer::GitWriter;
use gitpulse_lib::procguard::LockedSpawn;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

mod common;

struct Fixture {
    _dir: tempfile::TempDir,
    repo: PathBuf,
    repo_str: String,
}

fn fixture() -> Fixture {
    common::unmetered_spawns();
    let dir = tempfile::TempDir::new().unwrap();
    let repo = dir.path().canonicalize().unwrap();
    common::run_git(&repo, &["init", "-q"]);
    common::run_git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("keep.txt"), "keep\n").unwrap();
    common::run_git(&repo, &["add", "-A"]);
    common::run_git(&repo, &["commit", "-q", "-m", "init"]);
    let repo_str = repo.to_string_lossy().into_owned();
    Fixture {
        _dir: dir,
        repo,
        repo_str,
    }
}

/// `sub/.git` is a FILE naming `.modules/sub`, which holds a commit.
fn separate_gitdir(f: &Fixture) {
    std::fs::create_dir_all(f.repo.join("sub")).unwrap();
    std::fs::create_dir_all(f.repo.join(".modules")).unwrap();
    common::run_git(
        &f.repo.join("sub"),
        &["init", "-q", "--separate-git-dir", "../.modules/sub"],
    );
    std::fs::write(f.repo.join("sub/lib.txt"), "x\n").unwrap();
    common::run_git(&f.repo.join("sub"), &["add", "-A"]);
    common::run_git(
        &f.repo.join("sub"),
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "nested"],
    );
}

fn nested_head_commit(repo: &Path, dir: &str) -> bool {
    std::process::Command::new("git")
        .args(["rev-parse", "--verify", "-q", "HEAD"])
        .current_dir(repo.join(dir))
        .output_locked()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// BREAK (priority): deleting the folder that holds a nested gitdir. The
/// upward probe never sees it; `git clean -f -d` only spares a directory with
/// a `.git` inside, and a bare-style gitdir has none.
#[test]
fn deleting_the_folder_that_holds_a_nested_gitdir_is_refused() {
    let f = fixture();
    separate_gitdir(&f);
    let result = GitWriter::delete_path(&f.repo_str, ".modules", |_| Ok(()));
    assert!(
        f.repo.join(".modules/sub/HEAD").exists() && nested_head_commit(&f.repo, "sub"),
        "nested history gone after delete: {result:?}"
    );
}

/// BREAK: moving that folder leaves `sub/.git` pointing at nothing.
#[test]
fn moving_the_folder_that_holds_a_nested_gitdir_is_refused() {
    let f = fixture();
    separate_gitdir(&f);
    let result = GitWriter::move_path(&f.repo_str, ".modules", "moved", |_| Ok(()));
    assert!(
        nested_head_commit(&f.repo, "sub"),
        "nested repo broken by move: {result:?}"
    );
}

/// Holds: a nested repository whose `.git` is a full directory survives a
/// delete of its parent (git clean leaves nested repositories alone).
#[test]
fn deleting_the_parent_of_a_nested_repo_with_a_dot_git_directory_keeps_its_history() {
    let f = fixture();
    std::fs::create_dir(f.repo.join("sub")).unwrap();
    common::run_git(&f.repo.join("sub"), &["init", "-q"]);
    std::fs::write(f.repo.join("sub/a.txt"), "a").unwrap();
    common::run_git(&f.repo.join("sub"), &["add", "-A"]);
    common::run_git(
        &f.repo.join("sub"),
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "n"],
    );
    let _ = GitWriter::delete_path(&f.repo_str, "sub", |_| Ok(()));
    assert!(nested_head_commit(&f.repo, "sub"));
}

/// Holds: a `.git` file's directory deleted as the parent (sub/ with a gitfile).
#[test]
fn deleting_a_parent_whose_dot_git_is_a_file_leaves_the_repo_resolvable() {
    let f = fixture();
    separate_gitdir(&f);
    let _ = GitWriter::delete_path(&f.repo_str, "sub", |_| Ok(()));
    assert!(f.repo.join(".modules/sub/HEAD").exists());
}

/// A move of a nested repository with a real `.git` directory carries its
/// history with it (not a break; recorded for the report).
#[test]
fn moving_a_nested_repo_with_a_dot_git_directory_keeps_its_history() {
    let f = fixture();
    std::fs::create_dir(f.repo.join("sub")).unwrap();
    common::run_git(&f.repo.join("sub"), &["init", "-q"]);
    std::fs::write(f.repo.join("sub/a.txt"), "a").unwrap();
    common::run_git(&f.repo.join("sub"), &["add", "-A"]);
    common::run_git(
        &f.repo.join("sub"),
        &["-c", "commit.gpgsign=false", "commit", "-q", "-m", "n"],
    );
    GitWriter::move_path(&f.repo_str, "sub", "moved", |_| Ok(())).unwrap();
    assert!(nested_head_commit(&f.repo, "moved"));
}

/// Decoy: a directory with a fake HEAD/objects/refs blocks every write into
/// it. Pinned as the current (conservative) behaviour: repository content can
/// make a normal path unwritable from the editor, never the other way round.
#[test]
fn a_decoy_gitdir_blocks_writes_into_it() {
    let f = fixture();
    let decoy = f.repo.join("fixtures/bare.git");
    std::fs::create_dir_all(decoy.join("objects")).unwrap();
    std::fs::create_dir_all(decoy.join("refs")).unwrap();
    std::fs::write(decoy.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    assert!(sandbox_join_write(&f.repo, "fixtures/bare.git/notes.md").is_err());
    assert!(sandbox_create(&f.repo_str, "fixtures/bare.git/new.txt").is_err());
    // Siblings stay writable.
    assert!(sandbox_join_write(&f.repo, "fixtures/readme.md").is_ok());
}

/// A symlink to a gitdir is movable as a link; a write through it, or through
/// a symlinked ancestor, never reaches the gitdir.
#[test]
fn symlinks_to_a_nested_gitdir_are_never_written_through() {
    let f = fixture();
    separate_gitdir(&f);
    std::os::unix::fs::symlink(".modules/sub", f.repo.join("lnk")).unwrap();
    let head = std::fs::read(f.repo.join(".modules/sub/HEAD")).unwrap();
    assert!(sandbox_write(&f.repo_str, "lnk/HEAD", "x").is_err());
    assert!(sandbox_write(&f.repo_str, "lnk/config", "x").is_err());
    assert!(sandbox_create(&f.repo_str, "lnk/hooks/post-commit").is_err());
    assert!(sandbox_join_write_entry(&f.repo, "lnk/HEAD").is_err());
    // The link itself, as an entry, is allowed (it is the entry, not the dir).
    assert!(sandbox_join_write_entry(&f.repo, "lnk").is_ok());
    assert_eq!(
        std::fs::read(f.repo.join(".modules/sub/HEAD")).unwrap(),
        head
    );
    GitWriter::move_path(&f.repo_str, "lnk", "lnk2", |_| Ok(())).unwrap();
    assert!(f.repo.join(".modules/sub/HEAD").exists());
    // A chain: link to a link.
    std::os::unix::fs::symlink("lnk2", f.repo.join("lnk3")).unwrap();
    assert!(sandbox_write(&f.repo_str, "lnk3/HEAD", "x").is_err());
}

/// Other spellings of the ancestor names never write into the nested gitdir.
/// Where the filesystem folds case or Unicode normalization (APFS does, ext4
/// does not) a spelling *is* the gitdir and must be refused; where it does
/// not, it names an ordinary path beside it, and the gitdir is still
/// untouched. `.` and `..` resolve to the gitdir everywhere.
#[test]
fn case_and_unicode_variants_of_a_nested_gitdir_path_are_refused() {
    let f = fixture();
    separate_gitdir(&f);
    let gitdir = f.repo.join(".modules/sub");
    let before = |name: &str| std::fs::read(gitdir.join(name)).unwrap();
    let (head, config) = (before("HEAD"), before("config"));
    let folds_case = f.repo.join(".MODULES").exists();
    for (target, aliases) in [
        (".MODULES/sub/HEAD", folds_case),
        (".modules/SUB/HEAD", folds_case),
        (".Modules/Sub/config", folds_case),
        (".modules/sub/./HEAD", true),
        ("sub/../.modules/sub/HEAD", true),
    ] {
        let written = sandbox_write(&f.repo_str, target, "x");
        if aliases {
            assert!(written.is_err(), "{target}");
        }
    }
    assert_eq!(before("HEAD"), head, "a spelling wrote the nested HEAD");
    assert_eq!(
        before("config"),
        config,
        "a spelling wrote the nested config"
    );
    // NFC vs NFD directory name.
    let nfc = "caf\u{e9}.d";
    let nfd = "cafe\u{301}.d";
    let dir = f.repo.join(nfc);
    std::fs::create_dir_all(dir.join("objects")).unwrap();
    std::fs::create_dir_all(dir.join("refs")).unwrap();
    std::fs::write(dir.join("HEAD"), "ref: refs/heads/main\n").unwrap();
    let folds_normalization = f.repo.join(nfd).exists();
    for (name, aliases) in [(nfc, true), (nfd, folds_normalization)] {
        let target = format!("{name}/config");
        let written = sandbox_write(&f.repo_str, &target, "x");
        if aliases {
            assert!(written.is_err(), "{name}");
        }
    }
    assert!(
        !dir.join("config").exists(),
        "a spelling wrote a config into the gitdir {nfc}"
    );
}

/// BREAK: git's `is_ntfs_dotgit` stops at a backslash too (core.protectNTFS is
/// on by default everywhere, CVE-2019-1354 shape): one component named
/// `.git\hooks\pre-commit` is refused by `git add`/checkout, because it is
/// `.git/hooks/pre-commit` on Windows. The explorer creates it on unix.
#[test]
fn a_dot_git_backslash_component_is_refused_like_git_does() {
    for name in [
        ".git\\config",
        ".git\\hooks\\pre-commit",
        "git~1\\config",
        ".GIT\\x",
        ".git .\\x",
    ] {
        assert!(is_git_metadata_component(OsStr::new(name)), "{name:?}");
    }
}

/// Git refuses a symlinked `.gitmodules`; a rename is the one write that can
/// create one, so moving a link onto that name (or its NTFS/HFS spellings) is
/// refused, while a regular file may take the name.
#[test]
fn a_symlink_cannot_be_moved_onto_dot_gitmodules_but_a_file_can() {
    let f = fixture();
    std::os::unix::fs::symlink("keep.txt", f.repo.join("lnk")).unwrap();
    for to in [
        ".gitmodules",
        ".GITMODULES",
        "GITMOD~1",
        ".gitmodules ",
        ".gitmod\u{200c}ules",
    ] {
        let result = GitWriter::move_path(&f.repo_str, "lnk", to, |_| Ok(()));
        assert!(result.is_err(), "{to:?}: {result:?}");
        assert!(std::fs::symlink_metadata(f.repo.join("lnk")).is_ok());
    }
    std::fs::write(f.repo.join("plain"), "[submodule]\n").unwrap();
    GitWriter::move_path(&f.repo_str, "plain", ".gitmodules", |_| Ok(())).unwrap();
}

/// Over-refusal worth knowing about (not a break): `git~1` is refused on every
/// platform, so a user file named `git~1` cannot be created in the editor.
#[test]
fn git_tilde_one_is_refused_on_unix_too() {
    let f = fixture();
    assert!(sandbox_create(&f.repo_str, "git~1").is_err());
}

/// Overwrite never skips the stale-base guard and never reaches a gitdir.
#[test]
fn overwrite_cannot_bypass_the_base_or_the_gitdir_guard() {
    let f = fixture();
    separate_gitdir(&f);
    std::fs::write(f.repo.join("a.txt"), "one\n").unwrap();
    let stale = "0000000000000000000000000000000000000000";
    let err = sandbox_overwrite_edited_text(&f.repo_str, "a.txt", "mine\n", Some(stale))
        .expect_err("a base that is not the disk's is refused");
    assert!(
        matches!(err, FileWriteError::ChangedOnDisk { .. }),
        "{err:?}"
    );
    assert_eq!(std::fs::read(f.repo.join("a.txt")).unwrap(), b"one\n");
    for target in [".modules/sub/HEAD", "sub/.git", ".git/config"] {
        assert!(
            sandbox_overwrite_edited_text(&f.repo_str, target, "x", None).is_err(),
            "{target}"
        );
    }
}
