//! Adversarial tests for the base-relation fields (S3-11) against real git in
//! temporary repositories: `distance` stays the old formula, and `landed_on_base`,
//! `ahead`, `behind`, `merge_base` and `base_oid` are right on every way a
//! branch's work can reach, or fail to reach, the base.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

impl Repo {
    /// `git` with extra environment and without the success assertion; the
    /// exit status is returned, because a conflicting merge is a fixture.
    fn run(&self, env: &[(&str, &str)], args: &[&str]) -> bool {
        let mut command = Command::new("git");
        command.args(args).current_dir(self.path());
        for (key, value) in env {
            command.env(key, value);
        }
        command.output_locked().expect("git").status.success()
    }

    /// Write `content` to `file` and commit it, returning the sha.
    fn put(&self, file: &str, content: &str, message: &str) -> String {
        std::fs::write(self.path().join(file), content).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

// --- fixtures: a branch `f` whose work reaches `main` in each way ---------------

/// f edits root.txt, main edits it differently, `git merge --squash f`
/// conflicts, the human resolves it and commits.
fn squash_with_conflict_resolution() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("root.txt", "from f\n", "f edits root");
    repo.git(&["checkout", "main"]);
    repo.put("root.txt", "from main\n", "main edits root");
    assert!(!repo.run(&[], &["merge", "--squash", "f"]), "must conflict");
    std::fs::write(repo.path().join("root.txt"), "from main and f\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "squash f, resolved"]);
    (repo, tip)
}

/// A clean squash, then an edit folded into the squash commit.
fn squash_with_a_fixup() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--squash", "f"]);
    std::fs::write(repo.path().join("a.txt"), "work, fixed up\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "squash f with a fixup"]);
    (repo, tip)
}

/// f has c1, c2. Main gets c1 verbatim and c2 with an edit (a rebase merge
/// where the second commit was touched on the way in).
fn rebase_with_an_edited_commit() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "one\n", "c1");
    let tip = repo.put("b.txt", "two\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["cherry-pick", "f~1"]);
    repo.git(&["cherry-pick", "-n", "f"]);
    std::fs::write(repo.path().join("b.txt"), "two, edited\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "c2 edited"]);
    (repo, tip)
}

/// The control: both commits cherry-picked verbatim.
fn rebase_clean() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "one\n", "c1");
    let tip = repo.put("b.txt", "two\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["cherry-pick", "f~1", "f"]);
    (repo, tip)
}

/// Only the first of f's two commits is cherry-picked.
fn partial_cherry_pick() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "one\n", "c1");
    let tip = repo.put("b.txt", "two\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["cherry-pick", "f~1"]);
    (repo, tip)
}

/// f is fast-forwarded in, then reverted: its content is gone from main.
fn ff_then_reverted() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "f"]);
    repo.git(&["revert", "--no-edit", "HEAD"]);
    (repo, tip)
}

/// ... and then the revert is reverted (the reland): the content is back.
fn ff_reverted_and_relanded() -> (Repo, String) {
    let (repo, tip) = ff_then_reverted();
    repo.git(&["revert", "--no-edit", "HEAD"]);
    (repo, tip)
}

/// f merged with `--no-ff`, then the merge reverted with `-m 1`.
fn merge_then_reverted() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["merge", "--no-ff", "--no-edit", "f"]);
    repo.git(&["revert", "--no-edit", "-m", "1", "HEAD"]);
    (repo, tip)
}

/// `git merge -s ours f`: a merge commit that takes none of f's content.
fn merge_strategy_ours() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["merge", "-s", "ours", "--no-edit", "f"]);
    (repo, tip)
}

/// f points at the root commit; main has since moved: a branch with no commits
/// beyond the base.
fn empty_branch() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["branch", "f"]);
    repo.put("m.txt", "main moves\n", "main moves");
    let tip = repo.git(&["rev-parse", "f"]);
    (repo, tip)
}

/// The commit asked about is the base tip itself.
fn the_base_tip() -> (Repo, String) {
    let repo = Repo::new();
    let tip = repo.put("m.txt", "main moves\n", "main moves");
    (repo, tip)
}

/// f is cherry-picked onto main, and that cherry-pick is reverted.
fn rebase_then_reverted() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    repo.git(&["cherry-pick", "f"]);
    repo.git(&["revert", "--no-edit", "HEAD"]);
    (repo, tip)
}

/// f is squashed in, and the squash commit reverted.
fn squash_then_reverted() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--squash", "f"]);
    repo.git(&["commit", "-m", "squash f"]);
    repo.git(&["revert", "--no-edit", "HEAD"]);
    (repo, tip)
}

/// f holds one `--allow-empty` commit; nothing of it can be on any base.
fn only_an_empty_commit() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.git(&["commit", "--allow-empty", "-m", "empty"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    (repo, tip)
}

/// f's net diff is empty (a change and its undo); nothing is left to land.
fn net_empty_branch() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "work\n", "add a");
    repo.git(&["rm", "-q", "a.txt"]);
    repo.git(&["commit", "-m", "remove a"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "main moves\n", "main moves");
    (repo, tip)
}

#[allow(clippy::type_complexity)]
fn every_fixture() -> Vec<(&'static str, fn() -> (Repo, String))> {
    vec![
        (
            "squash_with_conflict_resolution",
            squash_with_conflict_resolution,
        ),
        ("squash_with_a_fixup", squash_with_a_fixup),
        ("rebase_with_an_edited_commit", rebase_with_an_edited_commit),
        ("rebase_clean", rebase_clean),
        ("partial_cherry_pick", partial_cherry_pick),
        ("ff_then_reverted", ff_then_reverted),
        ("ff_reverted_and_relanded", ff_reverted_and_relanded),
        ("merge_then_reverted", merge_then_reverted),
        ("merge_strategy_ours", merge_strategy_ours),
        ("empty_branch", empty_branch),
        ("the_base_tip", the_base_tip),
        ("rebase_then_reverted", rebase_then_reverted),
        ("squash_then_reverted", squash_then_reverted),
        ("only_an_empty_commit", only_an_empty_commit),
        ("net_empty_branch", net_empty_branch),
    ]
}

// --- 3. provenance fields -----------------------------------------------------------

/// The formula `distance` had before the change: `rev-list --count c..b`.
fn old_distance(repo: &Repo, commit: &str, base: &str) -> Option<u32> {
    let range = format!("{commit}..{base}");
    Command::new("git")
        .args(["rev-list", "--count", &range, "--"])
        .current_dir(repo.path())
        .output_locked()
        .ok()
        .filter(|out| out.status.success())
        .and_then(|out| String::from_utf8_lossy(&out.stdout).trim().parse().ok())
}

#[test]
fn distance_is_the_old_formula_on_every_fixture_and_the_relation_agrees() {
    for (name, build) in every_fixture() {
        let (repo, tip) = build();
        let f = compute_freshness(repo.as_str(), &tip, Some("main"));
        assert_eq!(
            f.distance,
            old_distance(&repo, &tip, "main"),
            "{name}: distance moved"
        );
        assert_eq!(f.is_fresh, f.distance == Some(0), "{name}");
        assert_eq!(f.on_base, f.landed_on_base, "{name}");
        if let (Some(behind), Some(distance)) = (f.behind, f.distance) {
            assert_eq!(behind, distance, "{name}: behind != distance");
        }
        assert!(
            f.landed_on_base.is_some() == f.base_relation_reason.is_empty(),
            "{name}: a null relation needs a reason and a measured one needs none: {f:?}"
        );
    }
}

#[test]
fn an_unrelated_history_commit_is_not_on_the_base_and_has_no_merge_base() {
    let repo = Repo::new();
    repo.put("m.txt", "m\n", "main moves");
    repo.git(&["checkout", "--orphan", "island"]);
    let island = repo.put("island.txt", "i\n", "island root");
    repo.git(&["checkout", "main"]);
    let f = compute_freshness(repo.as_str(), &island, Some("main"));
    assert_eq!(f.landed_on_base, Some(false), "{f:?}");
    assert_eq!(f.merge_base, None);
    assert_eq!(f.ahead, Some(1));
    assert_eq!(f.behind, Some(2));
    assert_eq!(f.distance, old_distance(&repo, &island, "main"));
}

#[test]
fn a_tag_a_detached_head_and_the_commit_itself_as_the_base() {
    let repo = Repo::new();
    let tip = repo.put("a.txt", "a\n", "a");
    repo.git(&["tag", "light"]);
    repo.git(&["tag", "-a", "-m", "annotated", "annot"]);
    let after = repo.put("b.txt", "b\n", "b");
    let oid_of_tip = tip.clone();
    for tag in ["light", "annot"] {
        let f = compute_freshness(repo.as_str(), &tip, Some(tag));
        assert_eq!(f.landed_on_base, Some(true), "{tag}: {f:?}");
        assert_eq!(f.ahead, Some(0), "{tag}");
        assert_eq!(f.behind, Some(0), "{tag}");
        assert_eq!(
            f.base_oid.as_deref(),
            Some(oid_of_tip.as_str()),
            "{tag}: base_oid must be the commit, not the tag object: {f:?}"
        );
        let later = compute_freshness(repo.as_str(), &after, Some(tag));
        assert_eq!(later.landed_on_base, Some(false), "{tag}: {later:?}");
        assert_eq!(later.ahead, Some(1), "{tag}");
        assert_eq!(
            later.base_oid.as_deref(),
            Some(oid_of_tip.as_str()),
            "{tag}"
        );
    }
    // base == commit.
    let f = compute_freshness(repo.as_str(), &after, Some(&after));
    assert_eq!(f.landed_on_base, Some(true));
    assert_eq!((f.ahead, f.behind), (Some(0), Some(0)));
    assert_eq!(f.merge_base.as_deref(), Some(after.as_str()));
    // Detached HEAD as the default base.
    repo.git(&["checkout", "-q", "--detach", &tip]);
    let f = compute_freshness(repo.as_str(), &after, None);
    assert_eq!(f.landed_on_base, Some(false), "{f:?}");
    assert_eq!(f.ahead, Some(1));
    let on_head = compute_freshness(repo.as_str(), &tip, None);
    assert_eq!(on_head.landed_on_base, Some(true), "{on_head:?}");
}

#[test]
fn a_shallow_clone_answers_reachable_commits_and_nulls_the_rest() {
    let origin = Repo::new();
    origin.git(&["checkout", "-b", "feature"]);
    let feature = origin.put("feature.txt", "f\n", "feature work");
    origin.git(&["checkout", "main"]);
    let old = origin.put("old.txt", "o\n", "old");
    let tip = origin.put("main.txt", "m\n", "main work");
    let clone = tempfile::tempdir().expect("tempdir");
    let url = format!("file://{}", origin.as_str());
    let cloned = Command::new("git")
        .args([
            "clone",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            clone.path().to_str().unwrap(),
        ])
        .output_locked()
        .expect("git clone");
    assert!(cloned.status.success(), "{cloned:?}");
    crate::test_support::trust_repo(clone.path());
    let path = clone.path().to_str().unwrap();

    // The tip is on the base even in a shallow clone.
    let f = compute_freshness(path, &tip, Some("origin/main"));
    assert_eq!(f.landed_on_base, Some(true), "{f:?}");
    // A commit below the shallow boundary does not exist here: null, never
    // false.
    let f = compute_freshness(path, &old, Some("origin/main"));
    assert_ne!(f.landed_on_base, Some(false), "{f:?}");
    // An unmerged branch tip in a shallow clone: null, with the reason.
    let f = compute_freshness(path, &feature, Some("origin/main"));
    assert_eq!(f.landed_on_base, None, "{f:?}");
    assert!(f.base_relation_reason.contains("shallow"), "{f:?}");
}

#[test]
fn a_partial_clone_does_not_fetch_objects_into_the_repository_on_a_read() {
    let origin = Repo::new();
    origin.git(&["config", "uploadpack.allowFilter", "true"]);
    origin.git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    origin.git(&["checkout", "-b", "f"]);
    let tip = origin.put("a.txt", "work\n", "f adds a");
    origin.git(&["checkout", "main"]);
    origin.put("m.txt", "m\n", "main moves");
    let clone = tempfile::tempdir().expect("tempdir");
    let url = format!("file://{}", origin.as_str());
    let cloned = Command::new("git")
        .args([
            "clone",
            "--filter=blob:none",
            "--no-checkout",
            "--no-single-branch",
            &url,
            clone.path().to_str().unwrap(),
        ])
        .output_locked()
        .expect("git clone");
    if !cloned.status.success() {
        println!("partial clone unavailable here: {cloned:?}");
        return;
    }
    crate::test_support::trust_repo(clone.path());
    let path = clone.path().to_str().unwrap();
    let count = |args: &[&str]| -> String {
        String::from_utf8_lossy(
            &Command::new("git")
                .args(args)
                .current_dir(path)
                .output_locked()
                .unwrap()
                .stdout,
        )
        .to_string()
    };
    let before = count(&["count-objects", "-v"]);
    let _ = compute_freshness(path, &tip, Some("origin/main"));
    let after = count(&["count-objects", "-v"]);
    assert_eq!(
        before, after,
        "compute_freshness reads only commits and refs; it must not fetch the \
         missing blobs of a partial clone into the object database"
    );
}
