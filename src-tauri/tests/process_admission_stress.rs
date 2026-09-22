//! Process admission under adversarial concurrency.
//!
//! The property under test is not "the gate returns the right answer" — the
//! unit tests cover that — but the one that survives a hostile filesystem
//! moving underneath it: **a child never runs in a directory that admission
//! did not approve.** Every test here keeps a thread rewriting the path while
//! spawns race against it, and asserts on where the children actually landed
//! rather than on what the gate returned.
//!
//! A spawn that *fails* under this pressure is a pass, and so is one that runs
//! in a plain directory the path legitimately resolved to. The single failure
//! is a child inside a repository that was never granted trust, because that
//! is the outcome the gate exists to prevent.
#![cfg(unix)]

use gitpulse_lib::procguard;
use std::io::Read;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// A world-writable holder is what puts admission on its strict path: the
/// holder's mode is what decides whether another principal could substitute
/// the entry, so this is the layout where the child-side re-anchor engages.
fn shared_holder_layout() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().canonicalize().expect("canonical base");
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o777)).expect("open holder");
    let admitted = root.join("admitted");
    let decoy = root.join("decoy");
    std::fs::create_dir(&admitted).expect("admitted");
    std::fs::create_dir(&decoy).expect("decoy");
    (base, admitted, decoy)
}

/// Runs `/bin/pwd` through the real admission seam and returns where it ran,
/// or `None` when the spawn was refused.
fn where_did_it_run(cwd: &Path) -> Option<String> {
    let mut cmd = Command::new("/bin/pwd");
    cmd.current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, registration) = procguard::spawn(&mut cmd, "admission-stress").ok()?;
    let mut out = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut out);
    }
    let _ = registration.reap(|| child.wait());
    Some(out.trim().to_string())
}

/// Flips `path` between the real directory and a symlink to `decoy` until
/// told to stop, so spawns race a name that keeps changing meaning.
fn substitution_storm(path: PathBuf, decoy: PathBuf, stop: Arc<AtomicBool>) -> Arc<AtomicUsize> {
    let flips = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&flips);
    std::thread::spawn(move || {
        let hidden = path.with_extension("hidden");
        while !stop.load(Ordering::Relaxed) {
            if std::fs::rename(&path, &hidden).is_ok() {
                let _ = std::os::unix::fs::symlink(&decoy, &path);
                counter.fetch_add(1, Ordering::Relaxed);
                std::thread::yield_now();
                let _ = std::fs::remove_file(&path);
                let _ = std::fs::rename(&hidden, &path);
            }
            std::thread::yield_now();
        }
        // Leave the real directory in place for the assertions.
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::rename(&hidden, &path);
    });
    flips
}

/// Turns `dir` into a Git repository that was never granted trust. Entering
/// one of these is the outcome the whole gate exists to prevent, which makes
/// it the only sound decoy: a plain directory is legitimately admissible, so
/// "the child ran in the decoy" would prove nothing.
fn untrusted_repository(dir: &Path) {
    let status = Command::new("git")
        .args(["init", "-q"])
        .arg(dir)
        .status()
        .expect("git init");
    assert!(status.success(), "could not build the decoy repository");
}

/// One storm. Returns `(ran, refused, escaped, flips)`.
///
/// The safety counts are the product property. `ran == 0` is about the test:
/// a runner that refused every spawn proved nothing, and the last `main` CI
/// run died on exactly that. The caller retries those; an escape fails at once.
fn exercise_substitution() -> (usize, usize, usize, usize) {
    let (_base, admitted, decoy) = shared_holder_layout();
    untrusted_repository(&decoy);
    let stop = Arc::new(AtomicBool::new(false));
    let flips = substitution_storm(admitted.clone(), decoy.clone(), Arc::clone(&stop));

    let escaped = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));
    let decoy_name = decoy.to_string_lossy().into_owned();

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let admitted = admitted.clone();
            let decoy_name = decoy_name.clone();
            let (escaped, ran, refused) =
                (Arc::clone(&escaped), Arc::clone(&ran), Arc::clone(&refused));
            scope.spawn(move || {
                for _ in 0..64 {
                    match where_did_it_run(&admitted) {
                        Some(landed) => {
                            ran.fetch_add(1, Ordering::Relaxed);
                            if landed == decoy_name {
                                escaped.fetch_add(1, Ordering::Relaxed);
                            }
                        }
                        None => {
                            refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    stop.store(true, Ordering::Relaxed);

    (
        ran.load(Ordering::Relaxed),
        refused.load(Ordering::Relaxed),
        escaped.load(Ordering::Relaxed),
        flips.load(Ordering::Relaxed),
    )
}

#[test]
fn a_relentless_substitution_never_walks_a_child_into_an_unapproved_repository() {
    let mut last = (0, 0, 0, 0);
    for _attempt in 0..4 {
        let (ran, refused, escaped, flips) = exercise_substitution();
        assert_eq!(
            escaped, 0,
            "{escaped} of {ran} children ran inside an unapproved repository \
             across {flips} substitutions"
        );
        assert_eq!(ran + refused, 8 * 64, "every attempt must be accounted for");
        if ran > 0 && flips > 0 {
            return;
        }
        last = (ran, refused, escaped, flips);
    }
    let (ran, _refused, _escaped, flips) = last;
    // About the test, not the fix: a storm that never flipped, or spawns that
    // were all refused, would have proven nothing. Four attempts is the budget;
    // past that the runner is not exercising the race.
    assert!(flips > 0, "the substitution thread never won a rename");
    assert!(
        ran > 0,
        "every spawn was refused; the race was never exercised"
    );
}

#[test]
fn admission_is_consistent_when_many_threads_resolve_the_same_path_at_once() {
    // The walk keeps per-call state (the descriptor chain, the running holder
    // metadata, the accumulated path). Shared state here would show up as
    // sporadic refusals or a child in the wrong place under contention, not as
    // a compile error.
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().canonicalize().expect("canonical");
    let dirs: Vec<PathBuf> = (0..8)
        .map(|i| {
            let d = root.join(format!("w{i}")).join("nested");
            std::fs::create_dir_all(&d).expect("nested");
            d
        })
        .collect();

    let wrong = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));
    std::thread::scope(|scope| {
        for dir in &dirs {
            let (wrong, refused) = (Arc::clone(&wrong), Arc::clone(&refused));
            scope.spawn(move || {
                for _ in 0..40 {
                    match where_did_it_run(dir) {
                        Some(landed) if landed == dir.to_string_lossy() => {}
                        Some(_) => {
                            wrong.fetch_add(1, Ordering::Relaxed);
                        }
                        None => {
                            refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    assert_eq!(
        wrong.load(Ordering::Relaxed),
        0,
        "a child ran in another thread's directory"
    );
    assert_eq!(
        refused.load(Ordering::Relaxed),
        0,
        "an ordinary private path was refused under contention"
    );
}

#[test]
fn admission_survives_a_path_at_and_beyond_the_component_limit() {
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().canonicalize().expect("canonical");
    // Comfortably inside the 256-component limit, and deep enough that a
    // per-component walk is doing real work.
    let mut deep = root.clone();
    for i in 0..120 {
        deep = deep.join(format!("d{i}"));
    }
    std::fs::create_dir_all(&deep).expect("deep tree");
    assert_eq!(
        where_did_it_run(&deep).as_deref(),
        Some(deep.to_string_lossy().as_ref()),
        "a legal deep path must still be admitted"
    );

    // Past the limit the walk must refuse rather than recurse or truncate.
    let mut past = deep.clone();
    for i in 0..200 {
        past = past.join(format!("x{i}"));
    }
    if std::fs::create_dir_all(&past).is_ok() {
        let mut cmd = Command::new("/bin/pwd");
        cmd.current_dir(&past).stdout(Stdio::null());
        let refusal = procguard::spawn(&mut cmd, "too-deep")
            .err()
            .map(|e| e.to_string())
            .unwrap_or_default();
        assert!(
            refusal.contains("depth limit"),
            "an over-deep path was not refused by the limit: {refusal:?}"
        );
    }
}

#[test]
fn a_symlinked_ancestor_is_admitted_every_time_under_load() {
    // Canonicalization runs before the no-follow walk precisely so ordinary
    // symlinked locations keep working. Under repetition, any drift here shows
    // up as an intermittent refusal rather than a clean failure.
    let base = tempfile::tempdir().expect("tempdir");
    let root = base.path().canonicalize().expect("canonical");
    let real = root.join("real").join("inner");
    std::fs::create_dir_all(&real).expect("real");
    let alias = root.join("alias");
    std::os::unix::fs::symlink(root.join("real"), &alias).expect("alias");
    let through_alias = alias.join("inner");

    for attempt in 0..200 {
        let landed = where_did_it_run(&through_alias)
            .unwrap_or_else(|| panic!("attempt {attempt} was refused"));
        assert_eq!(landed, real.to_string_lossy(), "attempt {attempt}");
    }
}

#[test]
fn repeated_admissions_do_not_leak_descriptors() {
    // Each admission opens one descriptor per path component and, on the
    // strict path, hands one to the command. A leak here would surface as a
    // process that stops being able to spawn at all after a few thousand
    // Git calls, which is a slow-burning outage rather than a test failure.
    let (_base, admitted, _decoy) = shared_holder_layout();
    let count_fds = || std::fs::read_dir("/dev/fd").map(|d| d.count()).unwrap_or(0);
    for _ in 0..20 {
        let _ = where_did_it_run(&admitted);
    }
    let settled = count_fds();
    for _ in 0..300 {
        let _ = where_did_it_run(&admitted);
    }
    let after = count_fds();
    assert!(
        after <= settled + 8,
        "descriptor count grew from {settled} to {after} over 300 admissions"
    );
}

/// Swaps `path` with `other` by rename, back and forth, until told to stop.
///
/// Deliberately not the symlink storm above: a symlink is refused by the
/// no-follow walk before anything else can happen, so it never exercises the
/// case where the pin lands on one real directory while the path re-resolves
/// to another.
fn rename_storm(path: PathBuf, other: PathBuf, stop: Arc<AtomicBool>) -> Arc<AtomicUsize> {
    let swaps = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&swaps);
    std::thread::spawn(move || {
        let stash = path.with_extension("stash");
        while !stop.load(Ordering::Relaxed) {
            if std::fs::rename(&path, &stash).is_ok() {
                if std::fs::rename(&other, &path).is_ok() {
                    counter.fetch_add(1, Ordering::Relaxed);
                    std::thread::yield_now();
                    let _ = std::fs::rename(&path, &other);
                }
                let _ = std::fs::rename(&stash, &path);
            }
            std::thread::sleep(std::time::Duration::from_micros(100));
        }
        let _ = std::fs::rename(&stash, &path);
    });
    swaps
}

/// Reports whether the child's *working directory* is the decoy, by looking
/// for a file that only the decoy contains.
///
/// Identity, not spelling: the storm keeps renaming both directories, so the
/// name a child sees says nothing about which inode it is standing in.
fn landed_in_decoy(cwd: &Path) -> Option<bool> {
    let mut cmd = Command::new("/bin/sh");
    cmd.arg("-c")
        .arg("if [ -e .this-is-the-decoy ]; then echo DECOY; else echo OK; fi")
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null());
    let (mut child, registration) = procguard::spawn(&mut cmd, "admission-rename-stress").ok()?;
    let mut out = String::new();
    if let Some(mut pipe) = child.stdout.take() {
        let _ = pipe.read_to_string(&mut out);
    }
    let _ = registration.reap(|| child.wait());
    Some(out.trim() == "DECOY")
}

/// Admission approves a *directory*, but it reads the path twice — once to pin
/// it, once to find the repository whose trust is then required. A rename that
/// lands between those two reads makes them describe different objects, and
/// the child is anchored to the first while the trust decision was made about
/// the second. Nothing in the symlink storm above can reach this: `O_NOFOLLOW`
/// refuses a symlink before the second read ever happens.
///
/// So the invariant is checked by identity rather than by name — the decoy
/// holds a file the admitted directory does not, and no child may ever see it.
#[test]
fn a_renamed_directory_cannot_split_the_pin_from_the_trust_decision() {
    let (_base, admitted, decoy) = shared_holder_layout();
    untrusted_repository(&decoy);
    std::fs::write(decoy.join(".this-is-the-decoy"), b"x").expect("decoy marker");

    let stop = Arc::new(AtomicBool::new(false));
    let swaps = rename_storm(admitted.clone(), decoy.clone(), Arc::clone(&stop));

    let escaped = Arc::new(AtomicUsize::new(0));
    let ran = Arc::new(AtomicUsize::new(0));
    let refused = Arc::new(AtomicUsize::new(0));

    std::thread::scope(|scope| {
        for _ in 0..8 {
            let admitted = admitted.clone();
            let (escaped, ran, refused) =
                (Arc::clone(&escaped), Arc::clone(&ran), Arc::clone(&refused));
            scope.spawn(move || {
                for _ in 0..128 {
                    match landed_in_decoy(&admitted) {
                        Some(true) => {
                            ran.fetch_add(1, Ordering::Relaxed);
                            escaped.fetch_add(1, Ordering::Relaxed);
                        }
                        Some(false) => {
                            ran.fetch_add(1, Ordering::Relaxed);
                        }
                        None => {
                            refused.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
            });
        }
    });
    stop.store(true, Ordering::Relaxed);

    let (ran, refused, escaped, swaps) = (
        ran.load(Ordering::Relaxed),
        refused.load(Ordering::Relaxed),
        escaped.load(Ordering::Relaxed),
        swaps.load(Ordering::Relaxed),
    );
    assert_eq!(
        escaped, 0,
        "{escaped} of {ran} children stood in the unapproved repository \
         across {swaps} renames"
    );
    assert_eq!(
        ran + refused,
        8 * 128,
        "every attempt must be accounted for"
    );
    // About the test, not the fix.
    assert!(swaps > 0, "the rename thread never won a swap");
    assert!(
        ran > 0,
        "every spawn was refused; the race was never exercised"
    );
}
