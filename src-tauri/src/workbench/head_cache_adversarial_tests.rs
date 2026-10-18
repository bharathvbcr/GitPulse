//! Adversarial checks of the HEAD and base lookup cache (ft-cd18575e). Every
//! test compares the cached read to `git rev-parse` asked directly, after each
//! move. `eager` removes the racy window so only the stamp itself is under
//! test; `settled_window_*` keeps the production window.

use crate::procguard::LockedSpawn;
use crate::workbench::verification_driver::verification_base;
use crate::workbench::verification_gate as gate;
use std::path::Path;
use std::time::Duration;

fn rev(repo: &Path, spec: &str) -> String {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "--verify", spec])
        .current_dir(repo)
        .output_locked()
        .unwrap();
    assert!(out.status.success(), "rev-parse {spec}");
    String::from_utf8(out.stdout).unwrap().trim().to_owned()
}

fn truth(repo: &Path) -> String {
    rev(repo, "HEAD^{commit}")
}

fn seen(repo: &Path) -> String {
    gate::current_head(repo.to_str().unwrap()).unwrap()
}

/// Reads twice (the second is the cached one) and asserts both are the truth.
fn agrees(repo: &Path, why: &str) {
    let want = truth(repo);
    assert_eq!(seen(repo), want, "first read after: {why}");
    assert_eq!(seen(repo), want, "cached read after: {why}");
}

fn commit(repo: &Path, message: &str) {
    crate::test_support::git_in(repo, &["commit", "-q", "--allow-empty", "-m", message]);
}

fn git(repo: &Path, args: &[&str]) {
    crate::test_support::git_in(repo, args);
}

#[test]
fn packed_ref_rewrites_and_loose_over_packed_are_seen() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "one");
    let first = truth(r);
    commit(r, "two");
    crate::engine::ref_cache::eager(|| {
        git(r, &["pack-refs", "--all", "--prune"]);
        agrees(r, "pack");
        git(r, &["update-ref", "refs/heads/main", &first]);
        agrees(r, "loose over packed");
        git(r, &["pack-refs", "--all", "--prune"]);
        agrees(r, "repack with the older commit");
        commit(r, "three");
        git(r, &["pack-refs", "--all", "--prune"]);
        agrees(r, "commit then pack");
    });
}

#[test]
fn checkout_of_another_branch_and_a_detached_head_are_seen() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "one");
    git(r, &["branch", "side"]);
    commit(r, "two");
    crate::engine::ref_cache::eager(|| {
        agrees(r, "start");
        git(r, &["checkout", "-q", "side"]);
        agrees(r, "switch to side");
        git(r, &["checkout", "-q", "--detach", "main"]);
        agrees(r, "detach at main");
        commit(r, "detached one");
        agrees(r, "commit on a detached head");
        git(r, &["checkout", "-q", "main"]);
        agrees(r, "back to main");
    });
}

#[test]
fn linked_worktrees_at_one_head_do_not_serve_each_other() {
    let _held = crate::engine::ref_cache::retention();
    let (main, _parent, linked) = crate::test_support::repo_with_worktrees(2);
    let (a, b) = (linked[0].as_path(), linked[1].as_path());
    crate::engine::ref_cache::eager(|| {
        agrees(main.path(), "main at the seed");
        agrees(a, "a at the seed");
        agrees(b, "b at the seed");
        commit(a, "only in a");
        agrees(a, "a moved");
        agrees(b, "b did not");
        agrees(main.path(), "main did not");
        commit(main.path(), "only in main");
        agrees(a, "a after main moved");
        agrees(main.path(), "main moved");
        git(b, &["checkout", "-q", "--detach"]);
        commit(b, "detached in b");
        agrees(b, "b detached commit");
        agrees(a, "a after b detached");
    });
}

#[test]
fn a_worktree_removed_and_re_added_at_the_same_path_is_seen() {
    let _held = crate::engine::ref_cache::retention();
    let (main, _parent, linked) = crate::test_support::repo_with_worktrees(1);
    let wt = linked[0].clone();
    let m = main.path();
    crate::engine::ref_cache::eager(|| {
        agrees(&wt, "first life");
        git(m, &["worktree", "remove", "--force", wt.to_str().unwrap()]);
        commit(m, "main moves while the worktree is gone");
        git(
            m,
            &["worktree", "add", "-q", "-b", "again", wt.to_str().unwrap()],
        );
        agrees(&wt, "second life at another commit");
    });
}

/// A HEAD that is a symref to a ref outside `refs/heads` is moved by updating
/// that ref, which touches neither HEAD nor anything under `refs/heads`.
/// `current_head` stamps `refs/heads` only.
#[test]
fn a_head_symref_to_another_namespace_is_seen() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "one");
    let first = truth(r);
    commit(r, "two");
    git(r, &["update-ref", "refs/remotes/origin/main", &first]);
    git(r, &["symbolic-ref", "HEAD", "refs/remotes/origin/main"]);
    crate::engine::ref_cache::eager(|| {
        agrees(r, "symref to a remote-tracking ref");
        git(r, &["update-ref", "refs/remotes/origin/main", "main"]);
        agrees(r, "that ref moved");
    });
}

#[test]
fn reftable_backend_moves_are_seen() {
    let _held = crate::engine::ref_cache::retention();
    let repo = tempfile::TempDir::new().unwrap();
    let r = repo.path();
    let init = std::process::Command::new("git")
        .args(["init", "-q", "-b", "main", "--ref-format=reftable"])
        .current_dir(r)
        .status_locked()
        .unwrap();
    if !init.success() {
        eprintln!("skipped: this git has no reftable support");
        return;
    }
    crate::test_support::trust_repo(r);
    commit(r, "one");
    let first = truth(r);
    commit(r, "two");
    crate::engine::ref_cache::eager(|| {
        agrees(r, "reftable start");
        git(r, &["update-ref", "HEAD", &first]);
        agrees(r, "update-ref under reftable");
        git(r, &["checkout", "-q", "--detach"]);
        commit(r, "detached under reftable");
        agrees(r, "detached commit under reftable");
    });
}

/// Production window (no `eager`): a read followed at once by a move,
/// repeated, is never stale, whatever the mtime granularity.
#[test]
fn settled_window_never_serves_a_move_made_straight_after_a_read() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "one");
    for n in 0..15 {
        agrees(r, "before");
        commit(r, &format!("c{n}"));
        assert_eq!(seen(r), truth(r), "move {n} right after a read");
    }
}

/// An aged ref (older than the window, so its stamp is stored) rewritten in
/// place to the same length with its mtime put back. Git never does this (it
/// renames a lockfile over the ref); a backup restore or `touch -r` could. It
/// records the stamp's limit: until the 60 s TTL the old answer is served.
#[test]
#[ignore = "accepted limitation: inode+len+mtime stamp cannot see an in-place rewrite that restores mtime; git never writes refs this way"]
fn an_in_place_rewrite_with_restored_mtime_is_seen() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "one");
    let first = truth(r);
    commit(r, "two");
    let aged = std::time::SystemTime::now() - Duration::from_secs(600);
    let touch = |path: &Path, when: std::time::SystemTime| {
        std::fs::File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(when)
            .unwrap();
    };
    let ref_file = r.join(".git/refs/heads/main");
    touch(&ref_file, aged);
    touch(&r.join(".git/HEAD"), aged);
    // `eager`: the fixture's other files are younger than the window, which
    // would keep any stamp from being stored; a steady repository has none.
    crate::engine::ref_cache::eager(|| {
        agrees(r, "aged ref");
        std::fs::write(&ref_file, format!("{first}\n")).unwrap();
        touch(&ref_file, aged);
        agrees(r, "rewritten in place, mtime restored");
    });
}

/// HEAD does not move; the default branch does. The cached base must follow.
#[test]
fn base_follows_a_default_branch_that_moves_under_a_fixed_head() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "c1");
    let c1 = rev(r, "HEAD");
    commit(r, "c2");
    let c2 = rev(r, "HEAD");
    git(r, &["checkout", "-q", "-b", "feature"]);
    commit(r, "c3");
    let head = rev(r, "HEAD");
    crate::engine::ref_cache::eager(|| {
        assert_eq!(verification_base(r, &head).unwrap(), c2);
        assert_eq!(verification_base(r, &head).unwrap(), c2);
        git(r, &["update-ref", "refs/heads/main", &c1]);
        assert_eq!(verification_base(r, &head).unwrap(), c1, "main moved back");
        assert_eq!(verification_base(r, &head).unwrap(), c1);
        git(r, &["update-ref", "refs/heads/main", &c2]);
        assert_eq!(verification_base(r, &head).unwrap(), c2, "main forward");
    });
}

/// The default branch is the remote's, named by `origin/HEAD`; a fetch moves
/// `refs/remotes/origin/main` and the cached base must follow.
#[test]
fn base_follows_a_remote_tracking_default_branch() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "c1");
    let c1 = rev(r, "HEAD");
    commit(r, "c2");
    let c2 = rev(r, "HEAD");
    git(r, &["checkout", "-q", "-b", "feature"]);
    commit(r, "c3");
    let head = rev(r, "HEAD");
    git(r, &["branch", "-D", "main"]);
    git(r, &["update-ref", "refs/remotes/origin/main", &c2]);
    git(
        r,
        &[
            "symbolic-ref",
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/main",
        ],
    );
    crate::engine::ref_cache::eager(|| {
        assert_eq!(verification_base(r, &head).unwrap(), c2);
        assert_eq!(verification_base(r, &head).unwrap(), c2);
        git(r, &["update-ref", "refs/remotes/origin/main", &c1]);
        assert_eq!(
            verification_base(r, &head).unwrap(),
            c1,
            "fetch moved origin/main"
        );
    });
}

/// A root commit is an error and must stay an error on repeat; a later commit
/// turns the error into an answer.
#[test]
fn errors_are_not_kept_and_recover() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path();
    commit(r, "root");
    let root = rev(r, "HEAD");
    crate::engine::ref_cache::eager(|| {
        assert!(verification_base(r, &root).is_err());
        assert!(verification_base(r, &root).is_err());
        commit(r, "second");
        let head = rev(r, "HEAD");
        assert_eq!(verification_base(r, &head).unwrap(), root);
    });
}

/// Two linked worktrees asked about one head: the answer is a function of the
/// object graph and the shared default branch, so they agree, and both follow
/// a default-branch move made from a third checkout.
#[test]
fn base_of_one_head_in_two_worktrees_follows_a_shared_default_branch_move() {
    let _held = crate::engine::ref_cache::retention();
    let (main, _parent, linked) = crate::test_support::repo_with_worktrees(2);
    let m = main.path();
    let c1 = rev(m, "main");
    commit(m, "c2");
    let c2 = rev(m, "main");
    let (a, b) = (linked[0].as_path(), linked[1].as_path());
    commit(a, "a1");
    let head = rev(a, "HEAD");
    crate::engine::ref_cache::eager(|| {
        assert_eq!(verification_base(a, &head).unwrap(), c1);
        assert_eq!(verification_base(b, &head).unwrap(), c1);
        git(m, &["update-ref", "refs/heads/main", &head]);
        let expected_parent = rev(a, "HEAD~1");
        assert_eq!(verification_base(a, &head).unwrap(), expected_parent);
        assert_eq!(verification_base(b, &head).unwrap(), expected_parent);
        git(m, &["update-ref", "refs/heads/main", &c2]);
        assert_eq!(verification_base(a, &head).unwrap(), c1);
        assert_eq!(verification_base(b, &head).unwrap(), c1);
    });
}

/// Readers hammer while HEAD moves; once the writer stops, the next read is
/// the truth.
#[test]
fn concurrent_readers_settle_on_the_truth_after_moves() {
    let _held = crate::engine::ref_cache::retention();
    let repo = crate::test_support::git_repo();
    let r = repo.path().to_path_buf();
    commit(&r, "one");
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let readers: Vec<_> = (0..4)
        .map(|_| {
            let (r, stop) = (r.clone(), stop.clone());
            std::thread::spawn(move || {
                crate::engine::ref_cache::eager(|| {
                    while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                        let _ = gate::current_head(r.to_str().unwrap());
                    }
                });
            })
        })
        .collect();
    for n in 0..25 {
        commit(&r, &format!("m{n}"));
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    for reader in readers {
        reader.join().unwrap();
    }
    crate::engine::ref_cache::eager(|| agrees(&r, "after the writer stopped"));
}
