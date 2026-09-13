//! File saves must not modify other names sharing a destination inode.
use gitpulse_lib::engine::git_cli::sandbox_write;
mod common;

#[test]
fn saving_a_hard_link_leaves_the_other_file_unchanged() {
    let fixture = tempfile::tempdir().unwrap();
    let repo = fixture.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    common::run_git(&repo, &["init", "-q"]);
    let other = fixture.path().join("other.txt");
    std::fs::write(&other, "preserve this file").unwrap();
    std::fs::hard_link(&other, repo.join("note.txt")).unwrap();
    sandbox_write(repo.to_str().unwrap(), "note.txt", "updated note").unwrap();
    assert_eq!(
        std::fs::read_to_string(&other).unwrap(),
        "preserve this file"
    );
    assert_eq!(
        std::fs::read_to_string(repo.join("note.txt")).unwrap(),
        "updated note"
    );
}

fn fixture() -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    common::run_git(repo.path(), &["init", "-q"]);
    repo
}

#[test]
fn nested_and_large_editor_saves_preserve_complete_content() {
    let repo = fixture();
    let root = repo.path().to_str().unwrap();
    sandbox_write(root, "new/deep/note.txt", "").unwrap();
    let contents = "x".repeat(gitpulse_lib::engine::budget::MAX_FILE_BYTES as usize);
    sandbox_write(root, "new/deep/note.txt", &contents).unwrap();
    assert_eq!(
        std::fs::read(repo.path().join("new/deep/note.txt")).unwrap(),
        contents.as_bytes()
    );
    sandbox_write(root, "new/deep/note.txt", "small again").unwrap();
    assert_eq!(
        std::fs::read_dir(repo.path().join("new/deep"))
            .unwrap()
            .count(),
        1,
        "successful saves must clean recovery artifacts"
    );
    let oversized = "x".repeat(gitpulse_lib::engine::budget::MAX_FILE_BYTES as usize + 1);
    assert!(sandbox_write(root, "new/deep/note.txt", &oversized)
        .unwrap_err()
        .contains("save limit"));
    assert_eq!(
        std::fs::read_to_string(repo.path().join("new/deep/note.txt")).unwrap(),
        "small again"
    );
}

#[cfg(unix)]
#[test]
fn saves_preserve_modes_and_internal_symlinks_and_refuse_read_only_files() {
    use std::os::unix::fs::PermissionsExt;
    let repo = fixture();
    let root = repo.path().to_str().unwrap();
    let real = repo.path().join("real");
    std::fs::write(&real, "old").unwrap();
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o750)).unwrap();
    std::os::unix::fs::symlink("real", repo.path().join("alias")).unwrap();
    sandbox_write(root, "alias", "new").unwrap();
    assert!(repo.path().join("alias").is_symlink());
    assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    assert_eq!(
        std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
        0o750
    );
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o400)).unwrap();
    if unsafe { libc::geteuid() } != 0 {
        assert!(sandbox_write(root, "real", "denied").is_err());
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "new");
    }
}

/// An ordinary content write drops set-user-ID and set-group-ID, and the
/// Linux metadata path strips them along with `security.capability`. macOS
/// reaches the same publication through `fcopyfile(COPYFILE_METADATA)`, which
/// copies the whole mode — so without an explicit mask a save would leave a
/// set-id file set-id on one platform only, which is the contract holding in
/// three places and failing in the fourth.
#[cfg(unix)]
#[test]
fn a_saved_file_keeps_its_permissions_but_never_its_set_id_bits() {
    use std::os::unix::fs::PermissionsExt;
    let repo = fixture();
    let root = repo.path().to_str().unwrap();
    let target = repo.path().join("tool");
    std::fs::write(&target, "old").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o4755)).unwrap();
    // Some filesystems refuse set-user-ID outright; nothing to prove if so.
    if std::fs::metadata(&target).unwrap().permissions().mode() & 0o4000 == 0 {
        eprintln!("SKIPPED: this filesystem does not keep set-user-ID");
        return;
    }
    sandbox_write(root, "tool", "new").unwrap();
    let mode = std::fs::metadata(&target).unwrap().permissions().mode();
    assert_eq!(
        std::fs::read_to_string(&target).unwrap(),
        "new",
        "the save itself must still land"
    );
    assert_eq!(
        mode & 0o7000,
        0,
        "a saved file kept a set-id or sticky bit: {:o}",
        mode
    );
    assert_eq!(
        mode & 0o777,
        0o755,
        "clearing set-id must not disturb the ordinary permission bits"
    );
}

#[test]
fn concurrent_saves_publish_whole_files_or_report_a_conflict() {
    let repo = fixture();
    let root = repo.path().to_str().unwrap();
    sandbox_write(root, "note", "seed").unwrap();
    let successes = std::sync::atomic::AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for worker in 0..8 {
            let successes = &successes;
            scope.spawn(move || {
                let content = worker.to_string().repeat(4096);
                for _ in 0..32 {
                    if sandbox_write(root, "note", &content).is_ok() {
                        successes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    }
                }
            });
        }
    });
    assert!(successes.load(std::sync::atomic::Ordering::Relaxed) > 0);
    let final_text = std::fs::read_to_string(repo.path().join("note")).unwrap();
    assert!(
        (0..8).any(|worker| final_text == worker.to_string().repeat(4096)),
        "a partial or mixed save was published"
    );
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn ancestor_changes_cannot_redirect_a_save_to_another_directory() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let repo = fixture();
    let outside = tempfile::tempdir().unwrap();
    let root = repo.path().to_str().unwrap();
    let folder = repo.path().join("folder");
    let parked = repo.path().join("parked");
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("note"), "inside").unwrap();
    std::fs::write(outside.path().join("note"), "outside sentinel").unwrap();
    std::os::unix::fs::symlink(outside.path(), &parked).unwrap();
    let a = CString::new(folder.as_os_str().as_bytes()).unwrap();
    let b = CString::new(parked.as_os_str().as_bytes()).unwrap();
    let exchange = || {
        #[cfg(target_os = "macos")]
        // SAFETY: both live, terminated paths name entries in this fixture.
        let status = unsafe {
            libc::renameatx_np(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_SWAP,
            )
        };
        #[cfg(target_os = "linux")]
        // SAFETY: both live, terminated paths name entries in this fixture.
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                a.as_ptr(),
                libc::AT_FDCWD,
                b.as_ptr(),
                libc::RENAME_EXCHANGE,
            )
        };
        assert_eq!(
            status,
            0,
            "fixture exchange failed: {}",
            std::io::Error::last_os_error()
        );
    };
    std::thread::scope(|scope| {
        scope.spawn(|| {
            for _ in 0..500 {
                exchange();
                std::thread::yield_now();
                exchange();
            }
        });
        for _ in 0..500 {
            let _result = sandbox_write(root, "folder/note", "inside update");
        }
    });
    assert_eq!(
        std::fs::read_to_string(outside.path().join("note")).unwrap(),
        "outside sentinel"
    );
    assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 1);
}
