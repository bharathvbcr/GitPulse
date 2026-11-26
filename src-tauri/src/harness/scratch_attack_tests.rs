//! Adversarial tests for [`super::scratch`]: names that nearly match, planted links,
//! unwritable trees, racing sweepers and the two caps.

use super::scratch::*;
use crate::procguard::LockedSpawn;
use std::path::Path;

const DEAD: u32 = 4_000_001;
const HASH: &str = "0123456789abcdef";

fn root(tag: &str) -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix(&format!("scratch-attack-{tag}-"))
        .tempdir()
        .expect("test root")
}

fn name(pid: u32) -> String {
    format!("{SCRATCH_PREFIX}{pid}-{HASH}")
}

fn names(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(root)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    found.sort();
    found
}

#[test]
fn near_miss_names_never_name_an_owner() {
    for bad in [
        // wrong hash length
        "gitpulse-harness-42-0123456789abcde",
        "gitpulse-harness-42-0123456789abcdef0",
        "gitpulse-harness-42-",
        // trailing dot / space / newline
        "gitpulse-harness-42-0123456789abcdef.",
        "gitpulse-harness-42-0123456789abcdef ",
        "gitpulse-harness-42-0123456789abcdef\n",
        // pid shapes
        "gitpulse-harness--0123456789abcdef",
        "gitpulse-harness-0-0123456789abcdef",
        "gitpulse-harness-4294967296-0123456789abcdef",
        "gitpulse-harness-99999999999999999999-0123456789abcdef",
        "gitpulse-harness-4 2-0123456789abcdef",
        // unicode digits and hex
        "gitpulse-harness-\u{664}\u{662}-0123456789abcdef",
        "gitpulse-harness-42-0123456789abcd\u{ff45}f",
        "gitpulse-harness-42-0123456789abcd\u{e9}",
        // prefix variants
        "Gitpulse-harness-42-0123456789abcdef",
        "gitpulse-harness-42-0123456789abcdef/..",
        "",
    ] {
        assert_eq!(owner_pid(bad), None, "{bad:?}");
    }
    assert_eq!(owner_pid(&name(u32::MAX)), Some(u32::MAX));
    assert_eq!(owner_pid(&name(1)), Some(1));
}

/// The sweep claims to remove only "the exact shape this code writes", and this
/// code writes `{pid}` with `{}` and the hash with `{:016x}`: a canonical decimal
/// pid and lowercase hex. A name that is not that was written by someone else.
#[test]
fn a_name_this_code_could_not_have_written_is_not_ours() {
    let accepted: Vec<&str> = [
        "gitpulse-harness-+42-0123456789abcdef",
        "gitpulse-harness-0042-0123456789abcdef",
        "gitpulse-harness-42-0123456789ABCDEF",
        "gitpulse-harness-42-0123456789abcDEF",
    ]
    .into_iter()
    .filter(|foreign| owner_pid(foreign).is_some())
    .collect();
    assert_eq!(
        accepted,
        Vec::<&str>::new(),
        "names scratch_dir cannot write were accepted as ours"
    );
}

#[cfg(unix)]
#[test]
fn files_links_and_foreign_trees_are_kept_and_nothing_outside_is_touched() {
    use std::os::unix::fs::symlink;
    let root = root("shapes");
    let outside = root.path().join("outside");
    std::fs::create_dir(&outside).unwrap();
    std::fs::write(outside.join("precious"), b"x").unwrap();

    // A regular file named like a scratch dir.
    std::fs::write(root.path().join(name(DEAD)), b"file").unwrap();
    // A symlink to a directory, to a missing target, and to the filesystem root.
    symlink(&outside, root.path().join(name(DEAD + 1))).unwrap();
    symlink("/definitely/not/here", root.path().join(name(DEAD + 2))).unwrap();
    symlink("/", root.path().join(name(DEAD + 3))).unwrap();
    // A real stale dir with a link inside that points outside.
    let stale = root.path().join(name(DEAD + 4));
    std::fs::create_dir(&stale).unwrap();
    symlink(&outside, stale.join("escape")).unwrap();
    symlink(outside.join("precious"), stale.join("escape-file")).unwrap();

    let swept = sweep_stale(root.path(), current_uid(), |_| false);

    assert!(!stale.exists(), "the real stale directory goes");
    assert!(
        outside.join("precious").is_file(),
        "a link inside was followed"
    );
    assert_eq!(
        std::fs::read(root.path().join(name(DEAD))).unwrap(),
        b"file"
    );
    for i in 1..=3 {
        assert!(
            std::fs::symlink_metadata(root.path().join(name(DEAD + i)))
                .unwrap()
                .file_type()
                .is_symlink(),
            "link {i} was removed or replaced"
        );
    }
    assert_eq!((swept.removed, swept.kept), (1, 4), "{swept:?}");
}

#[cfg(unix)]
#[test]
fn a_read_only_subtree_is_kept_and_reported_not_removed_halfway_silently() {
    use std::os::unix::fs::PermissionsExt;
    let root = root("readonly");
    let stale = root.path().join(name(DEAD));
    let locked = stale.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::write(locked.join("f"), b"x").unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
    // Root can unlink anyway; the test is about the unprivileged case.
    let privileged = current_uid() == 0;

    let swept = sweep_stale(root.path(), current_uid(), |_| false);

    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    if !privileged {
        assert_eq!((swept.removed, swept.kept), (0, 1), "{swept:?}");
        assert!(stale.exists());
        // Once writable, the next sweep takes it.
        let again = sweep_stale(root.path(), current_uid(), |_| false);
        assert_eq!(again.removed, 1, "{again:?}");
        assert!(!stale.exists());
    }
}

#[cfg(unix)]
#[test]
fn two_sweepers_racing_neither_fail_nor_leave_anything() {
    for round in 0..8 {
        let root = root("race");
        for i in 0..40u32 {
            let dir = root.path().join(name(DEAD + i));
            std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
            for f in 0..10 {
                std::fs::write(dir.join(format!("a/b/c/f{f}")), b"x").unwrap();
            }
        }
        let (one, two) = std::thread::scope(|scope| {
            let a = scope.spawn(|| sweep_stale(root.path(), current_uid(), |_| false));
            let b = scope.spawn(|| sweep_stale(root.path(), current_uid(), |_| false));
            (a.join().unwrap(), b.join().unwrap())
        });
        assert_eq!(names(root.path()), Vec::<String>::new(), "round {round}");
        assert_eq!(
            one.kept, 0,
            "round {round}: a lost race was reported as a failure: {one:?}"
        );
        assert_eq!(
            two.kept, 0,
            "round {round}: a lost race was reported as a failure: {two:?}"
        );
    }
}

#[test]
fn the_scan_cap_is_exact_and_only_set_when_something_was_left_unread() {
    let root = root("scan");
    for i in 0..MAX_SCANNED {
        std::fs::write(root.path().join(format!("junk-{i}")), b"").unwrap();
    }
    let swept = sweep_stale(root.path(), current_uid(), |_| false);
    assert_eq!(
        (swept.scanned, swept.capped),
        (MAX_SCANNED, false),
        "{swept:?}"
    );

    std::fs::write(root.path().join("one-more"), b"").unwrap();
    let swept = sweep_stale(root.path(), current_uid(), |_| false);
    assert_eq!(
        (swept.scanned, swept.capped),
        (MAX_SCANNED, true),
        "{swept:?}"
    );
}

#[test]
fn the_removal_cap_is_exact() {
    let exactly = root("rm-exact");
    for i in 0..MAX_REMOVED as u32 {
        std::fs::create_dir(exactly.path().join(name(DEAD + i))).unwrap();
    }
    let swept = sweep_stale(exactly.path(), current_uid(), |_| false);
    assert_eq!(
        (swept.removed, swept.capped),
        (MAX_REMOVED, false),
        "{swept:?}"
    );
    assert_eq!(names(exactly.path()), Vec::<String>::new());

    let over = root("rm-over");
    for i in 0..MAX_REMOVED as u32 + 1 {
        std::fs::create_dir(over.path().join(name(DEAD + i))).unwrap();
    }
    let swept = sweep_stale(over.path(), current_uid(), |_| false);
    assert_eq!(
        (swept.removed, swept.capped),
        (MAX_REMOVED, true),
        "{swept:?}"
    );
    assert_eq!(names(over.path()).len(), 1);
    // The next sweep finishes the job.
    let swept = sweep_stale(over.path(), current_uid(), |_| false);
    assert_eq!((swept.removed, swept.capped), (1, false), "{swept:?}");
}

/// When the cap is reached by exactly the last removable directory and what
/// follows is unrelated, the sweep did not leave anything of ours unexamined.
#[test]
fn hitting_the_removal_cap_on_the_last_stale_dir_is_not_reported_as_capped() {
    let root = root("rm-tail");
    for i in 0..MAX_REMOVED as u32 {
        std::fs::create_dir(root.path().join(name(DEAD + i))).unwrap();
    }
    std::fs::write(root.path().join("unrelated"), b"").unwrap();
    let swept = sweep_stale(root.path(), current_uid(), |_| false);
    assert_eq!(swept.removed, MAX_REMOVED);
    assert!(
        !swept.capped,
        "nothing stale was left, yet the sweep says it was cut short: {swept:?}"
    );
}

#[cfg(unix)]
#[test]
fn this_processs_own_dir_and_a_live_strangers_are_kept_by_the_real_liveness_check() {
    let root = root("live");
    let own = root.path().join(name(std::process::id()));
    std::fs::create_dir(&own).unwrap();
    // An unrelated live process whose pid a recycled directory name could carry.
    let mut stranger = std::process::Command::new("sleep")
        .arg("60")
        .spawn_locked()
        .expect("sleep");
    let theirs = root.path().join(name(stranger.id()));
    std::fs::create_dir(&theirs).unwrap();

    let swept = sweep_stale(root.path(), current_uid(), pid_alive);
    assert!(own.is_dir() && theirs.is_dir(), "{swept:?}");
    assert_eq!((swept.removed, swept.kept), (0, 2));

    stranger.kill().unwrap();
    stranger.wait().unwrap();
    let swept = sweep_stale(root.path(), current_uid(), pid_alive);
    assert!(
        !theirs.exists(),
        "a reaped owner's directory stays: {swept:?}"
    );
    assert!(own.is_dir());
}

#[cfg(unix)]
#[test]
fn liveness_edges_never_read_as_gone_when_unaddressable() {
    assert!(pid_alive(1), "init is signalable-or-EPERM, never ESRCH");
    assert!(pid_alive(i32::MAX as u32 + 1));
    assert!(pid_alive(u32::MAX));
}
