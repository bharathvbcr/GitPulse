//! Adversarial review of the write primitives (ft-f16fad35). Each test names
//! the attack and fails against the implementation if the break is real.

use super::*;

/// The digest's lane step, copied so a collision can be built on purpose.
fn step(lane: u64, word: u64) -> u64 {
    const PRIME: u64 = 0x9E37_79B9_7F4A_7C15;
    (lane ^ word).wrapping_mul(PRIME).rotate_left(29)
}

fn tmp() -> (tempfile::TempDir, PathBuf) {
    let root = tempfile::TempDir::new().unwrap();
    let canonical = root.path().canonicalize().unwrap();
    (root, canonical)
}

/// ATTACK 2: the token is the only guard against silently overwriting an
/// external edit. Each lane is a chain of invertible steps, so changing the
/// word in block 0 and solving for the word in block 1 of the same lane leaves
/// the lane, and so the token, unchanged: two different files of equal length
/// with one token, built in O(1).
#[test]
fn a_two_word_same_lane_edit_no_longer_collides() {
    let lane0: u64 = 0xcbf2_9ce4_8422_2325;
    let a = vec![0u8; 64];
    let first_b: u64 = 1;
    let second_b = step(lane0, first_b) ^ step(lane0, 0);
    let mut b = a.clone();
    b[0..8].copy_from_slice(&first_b.to_le_bytes());
    b[32..40].copy_from_slice(&second_b.to_le_bytes());
    assert_ne!(a, b);
    assert_eq!(a.len(), b.len());
    assert_ne!(
        content_token(Some(&a)),
        content_token(Some(&b)),
        "two different 64-byte files share one token: a stale base would be accepted"
    );
}

/// ATTACK 4: a save whose parent directory is a symlink out of the tree must
/// not write through it, even when the caller skips the canonical pre-check.
#[test]
fn save_does_not_write_through_a_symlinked_parent() {
    let (_g, root) = tmp();
    let outside = tempfile::TempDir::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), root.join("d")).unwrap();
    let result = write_regular_expecting(&root.join("d/f.txt"), Expect::Any, |_| Ok(b"x".to_vec()));
    assert!(
        result.is_err() && !outside.path().join("f.txt").exists(),
        "wrote through a symlinked parent: {result:?}"
    );
}

/// ATTACK 4: the parent directory is swapped for a symlink between the
/// expectation check and the publish.
#[test]
fn save_does_not_follow_a_parent_swapped_for_a_symlink_after_the_check() {
    let (_g, root) = tmp();
    let outside = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(root.join("d")).unwrap();
    std::fs::write(root.join("d/f.txt"), b"one").unwrap();
    let (r, o) = (root.clone(), outside.path().to_path_buf());
    let _ = write_regular_after_check(
        &root.join("d/f.txt"),
        Expect::Any,
        |_| Ok(b"two".to_vec()),
        move || {
            std::fs::rename(r.join("d"), r.join("d_old")).unwrap();
            std::os::unix::fs::symlink(&o, r.join("d")).unwrap();
        },
    );
    assert!(
        !outside.path().join("f.txt").exists(),
        "the publish followed a symlink swapped in after the check"
    );
}

/// ATTACK 4: a save keeps the file's permission bits.
#[test]
fn save_keeps_the_permission_bits() {
    use std::os::unix::fs::PermissionsExt;
    let (_g, root) = tmp();
    let f = root.join("run.sh");
    std::fs::write(&f, b"#!/bin/sh\n").unwrap();
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o750)).unwrap();
    write_regular_expecting(&f, Expect::Any, |_| Ok(b"#!/bin/sh\necho\n".to_vec())).unwrap();
    let mode = std::fs::metadata(&f).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o750, "mode after save was {mode:o}");
}

/// ATTACK 3: a dangling symlink at the create path is "something already
/// there" and is AlreadyExists, like a live one.
#[test]
fn create_over_a_dangling_symlink_is_already_exists() {
    let (_g, root) = tmp();
    std::os::unix::fs::symlink(root.join("nowhere"), root.join("l")).unwrap();
    let result = write_regular_expecting(&root.join("l"), Expect::Absent, |_| Ok(Vec::new()));
    assert_eq!(result, Err(WriteFailure::AlreadyExists));
    assert!(!root.join("nowhere").exists(), "created through the link");
}

/// ATTACK 3: many creators of one path: one winner, the rest typed.
#[test]
fn concurrent_creates_of_one_path_have_one_winner_and_typed_losers() {
    let (_g, root) = tmp();
    let target = root.join("a/b/new.txt");
    let handles: Vec<_> = (0..16)
        .map(|_| {
            let t = target.clone();
            std::thread::spawn(move || {
                write_regular_expecting(&t, Expect::Absent, |_| Ok(Vec::new()))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let wins = results.iter().filter(|r| r.is_ok()).count();
    let typed = results
        .iter()
        .filter(|r| matches!(r, Err(WriteFailure::AlreadyExists)))
        .count();
    assert_eq!((wins, typed), (1, 15), "{results:?}");
}

/// ATTACK 3: creators racing to make the same intermediate directories all
/// succeed.
#[test]
fn concurrent_creates_racing_on_intermediate_dirs_all_succeed() {
    let (_g, root) = tmp();
    let handles: Vec<_> = (0..16)
        .map(|i| {
            let t = root.join(format!("x/y/z/f{i}"));
            std::thread::spawn(move || {
                write_regular_expecting(&t, Expect::Absent, |_| Ok(Vec::new()))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    assert!(results.iter().all(|r| r.is_ok()), "{results:?}");
}

/// ATTACK 3: a create that fails (name too long) leaves no empty parents.
#[test]
fn a_failed_create_leaves_no_parent_directories() {
    let (_g, root) = tmp();
    let long = "n".repeat(300);
    let result = write_regular_expecting(
        &root.join(format!("fresh/dir/{long}")),
        Expect::Absent,
        |_| Ok(Vec::new()),
    );
    assert!(result.is_err());
    assert!(
        !root.join("fresh").exists(),
        "left an empty `fresh/dir` behind"
    );
}

/// ATTACK 3: a parent that is a regular file fails closed and is left alone.
#[test]
fn create_under_a_file_parent_fails_and_keeps_the_file() {
    let (_g, root) = tmp();
    std::fs::write(root.join("f"), b"keep").unwrap();
    let result = write_regular_expecting(&root.join("f/child"), Expect::Absent, |_| Ok(Vec::new()));
    assert!(result.is_err());
    assert_eq!(std::fs::read(root.join("f")).unwrap(), b"keep");
}

/// ATTACK 2/4: the token returned for a CRLF file is the token of the bytes on
/// disk, so the next save carrying it does not conflict.
#[test]
fn crlf_save_returns_the_token_of_the_bytes_on_disk() {
    let (_g, root) = tmp();
    let f = root.join("c.txt");
    std::fs::write(&f, b"a\r\nb\r\n").unwrap();
    let base = content_token(Some(b"a\r\nb\r\n"));
    let saved = write_regular_expecting(&f, Expect::Token(&base), |existing| {
        crate::engine::text_shape::encode_edited_text("c.txt", existing, "a\nB\n")
    })
    .unwrap();
    let disk = std::fs::read(&f).unwrap();
    assert_eq!(disk, b"a\r\nB\r\n");
    assert_eq!(saved, content_token(Some(&disk)));
    write_regular_expecting(&f, Expect::Token(&saved), |_| Ok(b"z".to_vec())).unwrap();
}

/// ATTACK 4: Token against a deleted file is a conflict, not a recreate.
#[test]
fn token_save_of_a_deleted_file_does_not_recreate_it() {
    let (_g, root) = tmp();
    let f = root.join("gone.txt");
    std::fs::write(&f, b"x").unwrap();
    let base = content_token(Some(b"x"));
    std::fs::remove_file(&f).unwrap();
    let r = write_regular_expecting(&f, Expect::Token(&base), |_| Ok(b"y".to_vec()));
    assert!(matches!(r, Err(WriteFailure::Changed { .. })), "{r:?}");
    assert!(!f.exists());
}

/// ATTACK 4: Expect::Absent when the file now exists keeps its bytes.
#[test]
fn absent_save_over_an_existing_file_keeps_it() {
    let (_g, root) = tmp();
    let f = root.join("e.txt");
    std::fs::write(&f, b"mine").unwrap();
    let r = write_regular_expecting(&f, Expect::Absent, |_| Ok(Vec::new()));
    assert_eq!(r, Err(WriteFailure::AlreadyExists));
    assert_eq!(std::fs::read(&f).unwrap(), b"mine");
}

const UMASK_CHILD: &str = "GITPULSE_UMASK_CHILD";

/// Runs `body` under process umask `mask` in a child process. The umask is
/// process-wide, and every other test in the lib process creates files and
/// asserts their modes, so it is never changed here. The parent re-executes
/// this test binary on exactly `test` with the mask in the environment; the
/// child sets it and runs the body. A child that matched no test would pass
/// vacuously, so the parent also requires "1 passed" in its output.
fn under_umask(test: &str, mask: libc::mode_t, body: impl FnOnce()) {
    use crate::procguard::LockedSpawn;
    match std::env::var(UMASK_CHILD) {
        Ok(want) if want == format!("{mask:o}") => {
            // SAFETY: umask swaps the mask of this single-test child process.
            unsafe { libc::umask(mask) };
            body();
        }
        // The child was started for another mask of the same test.
        Ok(_) => {}
        Err(_) => {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    test,
                    "--include-ignored",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(UMASK_CHILD, format!("{mask:o}"))
                .output_locked()
                .unwrap();
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                output.status.success() && text.contains("1 passed"),
                "umask {mask:o} child failed:\n{text}"
            );
        }
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A new file follows the process umask, like `touch` and a git checkout.
#[test]
fn a_new_file_gets_the_umask_default_mode() {
    for (mask, want) in [(0o022, 0o644), (0o077, 0o600), (0o002, 0o664)] {
        under_umask(
            "diff::conflict_fs::adversarial_tests::a_new_file_gets_the_umask_default_mode",
            mask,
            || {
                let (_g, root) = tmp();
                let f = root.join("new.txt");
                write_regular_expecting(&f, Expect::Absent, |_| Ok(b"x".to_vec())).unwrap();
                assert_eq!(mode_of(&f), want, "umask {mask:o}");
            },
        );
    }
}

/// New folders made on the way to a new file follow the umask too.
#[test]
fn a_new_directory_gets_the_umask_default_mode() {
    for (mask, want) in [(0o022, 0o755), (0o077, 0o700), (0o002, 0o775)] {
        under_umask(
            "diff::conflict_fs::adversarial_tests::a_new_directory_gets_the_umask_default_mode",
            mask,
            || {
                let (_g, root) = tmp();
                write_regular_expecting(&root.join("a/b/new.txt"), Expect::Absent, |_| {
                    Ok(Vec::new())
                })
                .unwrap();
                assert_eq!(mode_of(&root.join("a")), want, "umask {mask:o}");
                assert_eq!(mode_of(&root.join("a/b")), want, "umask {mask:o}");
            },
        );
    }
}

/// An overwrite keeps the existing mode whatever the umask is.
#[test]
fn an_overwrite_keeps_the_existing_mode_under_any_umask() {
    use std::os::unix::fs::PermissionsExt;
    for mask in [0o022, 0o077] {
        under_umask(
            "diff::conflict_fs::adversarial_tests::an_overwrite_keeps_the_existing_mode_under_any_umask",
            mask,
            || {
                let (_g, root) = tmp();
                for mode in [0o755, 0o600, 0o640] {
                    let f = root.join(format!("f-{mode:o}"));
                    std::fs::write(&f, b"old").unwrap();
                    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
                    write_regular_expecting(&f, Expect::Any, |_| Ok(b"new".to_vec())).unwrap();
                    assert_eq!(std::fs::read(&f).unwrap(), b"new");
                    assert_eq!(mode_of(&f), mode, "umask {mask:o}, mode {mode:o}");
                }
            },
        );
    }
}

/// A write that fails after staging leaves nothing world-readable behind.
#[test]
fn a_failed_create_leaves_no_world_readable_temporary() {
    under_umask(
        "diff::conflict_fs::adversarial_tests::a_failed_create_leaves_no_world_readable_temporary",
        0o022,
        || {
            let (_g, root) = tmp();
            let f = root.join("raced.txt");
            let racer = f.clone();
            let result = write_regular_after_check(
                &f,
                Expect::Absent,
                |_| Ok(b"secret".to_vec()),
                move || std::fs::write(&racer, b"other").unwrap(),
            );
            assert!(result.is_err(), "{result:?}");
            assert_eq!(std::fs::read(&f).unwrap(), b"other");
            for entry in std::fs::read_dir(&root).unwrap() {
                let entry = entry.unwrap();
                if entry.file_name() != "raced.txt" {
                    assert_eq!(mode_of(&entry.path()) & 0o077, 0, "{:?}", entry.path());
                }
            }
        },
    );
}

const ADV: &str = "diff::conflict_fs::adversarial_tests::";

fn missing() -> Worktree {
    Worktree {
        bytes: None,
        mode: "missing".into(),
    }
}

fn entry(bytes: &[u8], mode: &str) -> Worktree {
    Worktree {
        bytes: Some(bytes.to_vec()),
        mode: mode.into(),
    }
}

fn git(dir: &Path, args: &[&str]) {
    use crate::procguard::LockedSpawn;
    let out = std::process::Command::new("git")
        .args(["-c", "user.name=a", "-c", "user.email=a@example.invalid"])
        .args(args)
        .current_dir(dir)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output_locked()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// What `git checkout` makes of a `100755` entry under the current umask.
fn git_checkout_exec_mode(root: &Path) -> u32 {
    git(root, &["init", "-q"]);
    std::fs::write(root.join("tool.sh"), b"#!/bin/sh\n").unwrap();
    git(root, &["add", "tool.sh"]);
    git(root, &["update-index", "--chmod=+x", "tool.sh"]);
    git(root, &["commit", "-q", "-m", "x"]);
    std::fs::remove_file(root.join("tool.sh")).unwrap();
    git(root, &["checkout", "--", "tool.sh"]);
    mode_of(&root.join("tool.sh"))
}

fn exec_matches_git(mask: libc::mode_t, test: &str) {
    under_umask(&format!("{ADV}{test}"), mask, || {
        let (_g, root) = tmp();
        let oracle = git_checkout_exec_mode(&root);
        let (_h, other) = tmp();
        let f = other.join("tool.sh");
        replace(&f, &missing(), &entry(b"#!/bin/sh\n", "100755")).unwrap();
        assert_eq!(
            mode_of(&f),
            oracle,
            "umask {mask:o}: ours {:o}, git checkout {oracle:o}",
            mode_of(&f)
        );
    });
}

/// ATTACK 3: a new `100755` file matches what `git checkout` writes.
#[test]
fn a_new_executable_matches_git_checkout_under_common_umasks() {
    for mask in [0o022, 0o077, 0o027, 0o002] {
        exec_matches_git(
            mask,
            "a_new_executable_matches_git_checkout_under_common_umasks",
        );
    }
}

/// ATTACK 3: the executable bits are derived from the read bits rather than
/// taken from `0o777 & !umask`, so a umask that clears an execute bit but not
/// the matching read bit (026, 006, 011) gives a different mode from git's.
#[test]
#[ignore = "adv: exec bits derived from read bits diverge from git checkout (0777 & !umask) under umask 026/006/011"]
fn a_new_executable_matches_git_checkout_under_unusual_umasks() {
    for mask in [0o026, 0o006, 0o011] {
        exec_matches_git(
            mask,
            "a_new_executable_matches_git_checkout_under_unusual_umasks",
        );
    }
}

/// ATTACK 4: a new `120000` entry never chmods through the link.
#[test]
fn a_new_symlink_entry_does_not_chmod_its_target() {
    use std::os::unix::fs::PermissionsExt;
    under_umask(
        &format!("{ADV}a_new_symlink_entry_does_not_chmod_its_target"),
        0o022,
        || {
            let (_g, root) = tmp();
            let outside = root.join("outside.txt");
            std::fs::write(&outside, b"secret").unwrap();
            std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
            let link = root.join("link");
            replace(
                &link,
                &missing(),
                &entry(outside.to_str().unwrap().as_bytes(), "120000"),
            )
            .unwrap();
            assert!(std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(mode_of(&outside), 0o600);
        },
    );
}

/// ATTACK 5: a target deleted between the check and the publish must not
/// become a create with a wider mode: the save is refused.
#[test]
fn an_overwrite_whose_target_vanishes_is_refused_not_created() {
    under_umask(
        &format!("{ADV}an_overwrite_whose_target_vanishes_is_refused_not_created"),
        0o022,
        || {
            let (_g, root) = tmp();
            let f = root.join("f.txt");
            std::fs::write(&f, b"old").unwrap();
            let gone = f.clone();
            let result = write_regular_after_check(
                &f,
                Expect::Any,
                |_| Ok(b"new".to_vec()),
                move || std::fs::remove_file(&gone).unwrap(),
            );
            assert!(result.is_err(), "{result:?}");
            assert!(!f.exists(), "a vanished target was recreated");
        },
    );
}

/// ATTACK 5: a target swapped for a symlink in the same window is not written
/// through, and its destination is neither modified nor chmoded.
#[test]
fn an_overwrite_whose_target_becomes_a_symlink_leaves_the_destination() {
    use std::os::unix::fs::PermissionsExt;
    under_umask(
        &format!("{ADV}an_overwrite_whose_target_becomes_a_symlink_leaves_the_destination"),
        0o022,
        || {
            let (_g, root) = tmp();
            let f = root.join("f.txt");
            let outside = root.join("outside.txt");
            std::fs::write(&f, b"old").unwrap();
            std::fs::write(&outside, b"keep").unwrap();
            std::fs::set_permissions(&outside, std::fs::Permissions::from_mode(0o600)).unwrap();
            let (swap, dest) = (f.clone(), outside.clone());
            let result = write_regular_after_check(
                &f,
                Expect::Any,
                |_| Ok(b"new".to_vec()),
                move || {
                    std::fs::remove_file(&swap).unwrap();
                    std::os::unix::fs::symlink(&dest, &swap).unwrap();
                },
            );
            assert!(result.is_err(), "{result:?}");
            assert_eq!(std::fs::read(&outside).unwrap(), b"keep");
            assert_eq!(mode_of(&outside), 0o600);
        },
    );
}

/// ATTACK 1: a new-file save into a read-only folder fails exactly as it did
/// before the probe (the staged temporary cannot be created either), leaving
/// no probe behind.
#[test]
fn a_new_file_in_a_read_only_folder_fails_cleanly() {
    use std::os::unix::fs::PermissionsExt;
    let (_g, root) = tmp();
    let dir = root.join("ro");
    std::fs::create_dir(&dir).unwrap();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o555)).unwrap();
    if std::fs::write(dir.join("writable"), b"").is_ok() {
        return; // root ignores the mode; nothing to attack
    }
    let result = write_regular_expecting(&dir.join("n.txt"), Expect::Absent, |_| Ok(Vec::new()));
    assert!(result.is_err());
    assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// ATTACK 2: a folder with an inheritable ACL gives a saved new file the same
/// mode and the same inherited entries as a file created the ordinary way.
#[cfg(target_os = "macos")]
#[test]
fn a_new_file_in_an_acl_folder_matches_an_ordinary_create() {
    use crate::procguard::LockedSpawn;
    let (_g, root) = tmp();
    let run = |program: &str, args: &[&str]| {
        let out = std::process::Command::new(program)
            .args(args)
            .output_locked()
            .unwrap();
        assert!(out.status.success(), "{program} {args:?}");
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    let dir = root.join("acl");
    std::fs::create_dir(&dir).unwrap();
    run(
        "/bin/chmod",
        &[
            "+a",
            "group:everyone allow read,file_inherit",
            dir.to_str().unwrap(),
        ],
    );
    let plain = dir.join("plain.txt");
    std::fs::File::create(&plain).unwrap();
    let saved = dir.join("saved.txt");
    write_regular_expecting(&saved, Expect::Absent, |_| Ok(b"x".to_vec())).unwrap();
    // `ls -le` prints the mode column, then the inherited ACL lines.
    let describe = |path: &Path| {
        let listing = run("/bin/ls", &["-le", path.to_str().unwrap()]);
        let mut lines = listing.lines();
        let head = lines.next().unwrap_or_default();
        let mode: String = head.split_whitespace().next().unwrap_or_default().into();
        (mode, lines.map(str::trim).collect::<Vec<_>>().join("|"))
    };
    let (plain_mode, plain_acl) = describe(&plain);
    let (saved_mode, saved_acl) = describe(&saved);
    assert!(!plain_acl.is_empty(), "the folder ACL was not inherited");
    assert_eq!(
        saved_mode.trim_end_matches('+'),
        plain_mode.trim_end_matches('+')
    );
    assert_eq!(
        saved_acl.split_once(' ').map(|x| x.1),
        plain_acl.split_once(' ').map(|x| x.1)
    );
}
