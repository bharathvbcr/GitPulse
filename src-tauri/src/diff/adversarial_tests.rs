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

/// Sets the process umask for a test and restores it on drop. The umask is
/// process-wide, so the guard holds the env serial for its whole lifetime.
struct Umask {
    previous: libc::mode_t,
    _serial: crate::test_support::env::EnvGuard,
}

impl Umask {
    fn set(mask: libc::mode_t) -> Self {
        let serial = crate::test_support::env::env_serial();
        // SAFETY: umask only swaps the process mask and cannot fail.
        let previous = unsafe { libc::umask(mask) };
        Self {
            previous,
            _serial: serial,
        }
    }
}

impl Drop for Umask {
    fn drop(&mut self) {
        // SAFETY: restores the mask this guard replaced.
        unsafe { libc::umask(self.previous) };
    }
}

fn mode_of(path: &Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o777
}

/// A new file follows the process umask, like `touch` and a git checkout.
#[test]
fn a_new_file_gets_the_umask_default_mode() {
    let (_g, root) = tmp();
    for (mask, want) in [(0o022, 0o644), (0o077, 0o600), (0o002, 0o664)] {
        let _umask = Umask::set(mask);
        let f = root.join(format!("new-{mask:o}.txt"));
        write_regular_expecting(&f, Expect::Absent, |_| Ok(b"x".to_vec())).unwrap();
        assert_eq!(mode_of(&f), want, "umask {mask:o}");
    }
}

/// New folders made on the way to a new file follow the umask too.
#[test]
fn a_new_directory_gets_the_umask_default_mode() {
    let (_g, root) = tmp();
    {
        let _umask = Umask::set(0o022);
        write_regular_expecting(
            &root.join("a/b/new.txt"),
            Expect::Absent,
            |_| Ok(Vec::new()),
        )
        .unwrap();
        assert_eq!(mode_of(&root.join("a")), 0o755);
        assert_eq!(mode_of(&root.join("a/b")), 0o755);
    }
    let _tight = Umask::set(0o077);
    write_regular_expecting(&root.join("c/new.txt"), Expect::Absent, |_| Ok(Vec::new())).unwrap();
    assert_eq!(mode_of(&root.join("c")), 0o700);
    drop(_tight);
    let _loose = Umask::set(0o002);
    write_regular_expecting(&root.join("d/new.txt"), Expect::Absent, |_| Ok(Vec::new())).unwrap();
    assert_eq!(mode_of(&root.join("d")), 0o775);
}

/// An overwrite keeps the existing mode whatever the umask is.
#[test]
fn an_overwrite_keeps_the_existing_mode_under_any_umask() {
    use std::os::unix::fs::PermissionsExt;
    let (_g, root) = tmp();
    for mask in [0o022, 0o077] {
        let _umask = Umask::set(mask);
        for mode in [0o755, 0o600, 0o640] {
            let f = root.join(format!("f-{mask:o}-{mode:o}"));
            std::fs::write(&f, b"old").unwrap();
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(mode)).unwrap();
            write_regular_expecting(&f, Expect::Any, |_| Ok(b"new".to_vec())).unwrap();
            assert_eq!(std::fs::read(&f).unwrap(), b"new");
            assert_eq!(mode_of(&f), mode, "umask {mask:o}, mode {mode:o}");
        }
    }
}

/// The temporary the atomic write stages in is never world-readable, and a
/// write that fails after staging leaves nothing behind that is.
#[test]
fn a_failed_create_leaves_no_world_readable_temporary() {
    let (_g, root) = tmp();
    let _umask = Umask::set(0o022);
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
}
