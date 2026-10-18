//! `distance` is kept as measured and the relation to the base is additive;
//! `landed` recognises every way a commit reaches the base. Real git, in
//! temporary repositories.

use super::*;
use crate::procguard::LockedSpawn;
use std::process::Command;

/// A real repository on `main` with one root commit.
struct Repo(tempfile::TempDir);

impl Repo {
    fn new() -> Self {
        let repo = Repo(tempfile::tempdir().expect("tempdir"));
        repo.git(&["init", "-b", "main"]);
        repo.git(&["config", "user.name", "T"]);
        repo.git(&["config", "user.email", "t@e.com"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(repo.path());
        repo.commit("root", "root.txt");
        repo
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn as_str(&self) -> &str {
        self.path().to_str().expect("utf8")
    }

    fn git(&self, args: &[&str]) -> String {
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// A commit adding `file`, returning its sha.
    fn commit(&self, message: &str, file: &str) -> String {
        std::fs::write(self.path().join(file), format!("{message}\n")).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }
}

#[test]
fn a_commit_ahead_of_the_base_is_not_on_it_and_distance_is_unchanged() {
    let repo = Repo::new();
    let base = repo.git(&["rev-parse", "HEAD"]);
    let ahead = repo.commit("ahead", "ahead.txt");

    let f = compute_freshness(repo.as_str(), &ahead, Some(&base));

    // What `distance` measures, kept exactly: the base has nothing the commit
    // lacks, so it is 0 and the commit still reads as fresh.
    assert_eq!(f.distance, Some(0));
    assert!(f.is_fresh);
    // What it never said, now said.
    assert_eq!(f.landed_on_base, Some(false));
    assert_eq!(f.on_base, Some(false));
    assert_eq!(f.ahead, Some(1));
    assert_eq!(f.behind, Some(0));
    assert_eq!(f.base_oid.as_deref(), Some(base.as_str()));
    assert_eq!(f.merge_base.as_deref(), Some(base.as_str()));
    assert_eq!(f.base_relation_reason, "");
}

#[test]
fn a_merged_commit_is_on_the_base_and_behind_counts_what_followed_it() {
    let repo = Repo::new();
    let merged = repo.commit("merged", "merged.txt");
    repo.commit("after", "after.txt");

    let f = compute_freshness(repo.as_str(), &merged, Some("main"));

    assert_eq!(f.landed_on_base, Some(true));
    assert_eq!(f.on_base, Some(true));
    assert_eq!(f.ahead, Some(0));
    assert_eq!(f.behind, Some(1));
    assert_eq!(f.distance, f.behind);
    assert_eq!(f.merge_base.as_deref(), Some(merged.as_str()));
    assert_eq!(
        f.base_oid.as_deref(),
        Some(repo.git(&["rev-parse", "main"]).as_str())
    );
}

#[test]
fn an_unresolvable_base_is_unmeasured_with_a_reason_never_false() {
    let repo = Repo::new();
    let sha = repo.git(&["rev-parse", "HEAD"]);

    let f = compute_freshness(repo.as_str(), &sha, Some("no-such-branch"));

    assert_eq!(f.distance, None);
    assert_eq!(f.landed_on_base, None);
    assert_eq!(f.on_base, None);
    assert_eq!(f.ahead, None);
    assert_eq!(f.behind, None);
    assert!(
        f.base_relation_reason.contains("no-such-branch"),
        "{}",
        f.base_relation_reason
    );
}

#[test]
fn a_shallow_clone_does_not_answer_not_reachable() {
    let origin = Repo::new();
    origin.git(&["checkout", "-b", "feature"]);
    origin.commit("feature work", "feature.txt");
    origin.git(&["checkout", "main"]);
    origin.commit("main work", "main.txt");
    let feature = origin.git(&["rev-parse", "feature"]);

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

    let f = compute_freshness(path, &feature, Some("origin/main"));

    assert_eq!(f.landed_on_base, None, "{f:?}");
    assert!(
        f.base_relation_reason.contains("shallow"),
        "{}",
        f.base_relation_reason
    );
    let answer = landed(path, &feature, Some("origin/main"));
    assert_eq!(answer.landed, None);
    assert!(answer.error.contains("shallow"), "{}", answer.error);
}

#[test]
fn a_batch_row_does_not_claim_a_relation_it_did_not_measure() {
    let repo = Repo::new();
    let sha = repo.git(&["rev-parse", "HEAD"]);
    let rows = freshness_batch(repo.as_str(), &[sha], None);
    assert_eq!(rows[0].landed_on_base, None);
    assert!(!rows[0].base_relation_reason.is_empty());
}

#[test]
fn a_fast_forward_landing_is_ff_with_a_landing_time() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "feature"]);

    let answer = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.via, Some(LandedVia::Ff));
    assert_eq!(answer.merge_commit, None);
    assert!(answer.landed_at.is_some(), "{answer:?}");
    assert_eq!(answer.error, "");
}

#[test]
fn a_merge_landing_names_the_merge_commit() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);
    let merge = repo.git(&["rev-parse", "HEAD"]);
    repo.commit("later", "later.txt");

    let answer = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.via, Some(LandedVia::Merge));
    assert_eq!(answer.merge_commit.as_deref(), Some(merge.as_str()));
    assert!(answer.landed_at.is_some(), "{answer:?}");
}

#[test]
fn a_merge_that_followed_a_directly_landed_commit_does_not_make_it_a_merge() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    let direct = repo.commit("direct on main", "direct.txt");
    repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);

    let answer = landed(repo.as_str(), &direct, Some("main"));

    assert_eq!(answer.via, Some(LandedVia::Ff), "{answer:?}");
    assert_eq!(answer.merge_commit, None);
}

#[test]
fn a_squash_landing_names_the_squash_commit() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    repo.commit("first", "one.txt");
    let tip = repo.commit("second", "two.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    repo.git(&["merge", "--squash", "feature"]);
    repo.git(&["commit", "-m", "squashed feature"]);
    let squash = repo.git(&["rev-parse", "HEAD"]);

    let answer = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.via, Some(LandedVia::Squash));
    assert_eq!(answer.merge_commit.as_deref(), Some(squash.as_str()));
    assert!(answer.landed_at.is_some(), "{answer:?}");
    // Not reachable, and the freshness read says so rather than "landed".
    let f = compute_freshness(repo.as_str(), &tip, Some("main"));
    assert_eq!(f.landed_on_base, Some(false));
}

#[test]
fn a_rebase_landing_is_rebase() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    repo.commit("first", "one.txt");
    let original = repo.commit("second", "two.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    repo.git(&["checkout", "-b", "rebased", "feature"]);
    repo.git(&["rebase", "main"]);
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "rebased"]);

    let answer = landed(repo.as_str(), &original, Some("main"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.via, Some(LandedVia::Rebase));
    assert_eq!(answer.merge_commit, None);
    assert!(answer.landed_at.is_none());
    assert!(!answer.landed_at_reason.is_empty());
}

#[test]
fn an_unmerged_commit_is_false_not_unknown() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");

    let answer = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(answer.landed, Some(false), "{answer:?}");
    assert_eq!(answer.via, None);
    assert_eq!(answer.error, "");
}

#[test]
fn an_unresolvable_commit_or_base_is_null_with_an_error() {
    let repo = Repo::new();
    let sha = repo.git(&["rev-parse", "HEAD"]);

    let bad_base = landed(repo.as_str(), &sha, Some("no-such-branch"));
    assert_eq!(bad_base.landed, None);
    assert!(
        bad_base.error.contains("no-such-branch"),
        "{}",
        bad_base.error
    );

    let bad_commit = landed(repo.as_str(), "no-such-commit", None);
    assert_eq!(bad_commit.landed, None);
    assert!(!bad_commit.error.is_empty());
    assert_eq!(bad_commit.base_oid, None);
}

/// Every file under `.git/objects` (loose objects, pack files and their
/// indexes) with its bytes, in path order: the listing, the count and the
/// content in one value.
fn object_store(repo: &Repo) -> Vec<(String, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).expect("read_dir") {
            let path = entry.expect("entry").path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, std::fs::read(&path).expect("read")));
            }
        }
    }
    let root = repo.path().join(".git").join("objects");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

#[test]
fn landed_writes_nothing_to_the_object_database_on_any_landing_kind() {
    type Scenario = fn(&Repo) -> (String, Option<LandedVia>, Option<bool>);
    let scenarios: [(&str, Scenario); 5] = [
        ("ff", |repo| {
            repo.git(&["checkout", "-b", "feature"]);
            let tip = repo.commit("feature work", "feature.txt");
            repo.git(&["checkout", "main"]);
            repo.git(&["merge", "--ff-only", "feature"]);
            (tip, Some(LandedVia::Ff), Some(true))
        }),
        ("merge", |repo| {
            repo.git(&["checkout", "-b", "feature"]);
            let tip = repo.commit("feature work", "feature.txt");
            repo.git(&["checkout", "main"]);
            repo.commit("main work", "main.txt");
            repo.git(&["merge", "--no-ff", "feature", "-m", "merge feature"]);
            (tip, Some(LandedVia::Merge), Some(true))
        }),
        ("squash", |repo| {
            repo.git(&["checkout", "-b", "feature"]);
            repo.commit("first", "one.txt");
            let tip = repo.commit("second", "two.txt");
            repo.git(&["checkout", "main"]);
            repo.commit("main work", "main.txt");
            repo.git(&["merge", "--squash", "feature"]);
            repo.git(&["commit", "-m", "squashed feature"]);
            (tip, Some(LandedVia::Squash), Some(true))
        }),
        ("rebase", |repo| {
            repo.git(&["checkout", "-b", "feature"]);
            repo.commit("first", "one.txt");
            let tip = repo.commit("second", "two.txt");
            repo.git(&["checkout", "main"]);
            repo.commit("main work", "main.txt");
            repo.git(&["checkout", "-b", "rebased", "feature"]);
            repo.git(&["rebase", "main"]);
            repo.git(&["checkout", "main"]);
            repo.git(&["merge", "--ff-only", "rebased"]);
            (tip, Some(LandedVia::Rebase), Some(true))
        }),
        ("unmerged", |repo| {
            repo.git(&["checkout", "-b", "feature"]);
            let tip = repo.commit("feature work", "feature.txt");
            repo.git(&["checkout", "main"]);
            repo.commit("main work", "main.txt");
            (tip, None, Some(false))
        }),
    ];
    for (kind, build) in scenarios {
        let repo = Repo::new();
        let (tip, via, landed_flag) = build(&repo);
        // Loose objects are packed so a later write would show as a new file
        // in either place; the listing is taken after that.
        repo.git(&["gc", "--quiet"]);
        let before = object_store(&repo);
        assert!(
            !before.is_empty(),
            "{kind}: an empty object store proves nothing"
        );

        let answer = landed(repo.as_str(), &tip, Some("main"));
        let _ = compute_freshness(repo.as_str(), &tip, Some("main"));

        assert_eq!(answer.landed, landed_flag, "{kind}: {answer:?}");
        assert_eq!(answer.via, via, "{kind}: {answer:?}");
        let after = object_store(&repo);
        assert_eq!(
            before.len(),
            after.len(),
            "{kind}: object file count changed"
        );
        assert!(before == after, "{kind}: the object store changed");
    }
}

#[test]
fn landing_changes_no_ref_or_working_file() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    let refs = || repo.git(&["for-each-ref", "--format=%(refname) %(objectname)"]);
    let status = || repo.git(&["status", "--porcelain=v1"]);
    let (refs_before, status_before) = (refs(), status());

    let _ = landed(repo.as_str(), &tip, Some("main"));
    let _ = compute_freshness(repo.as_str(), &tip, Some("main"));

    assert_eq!(refs(), refs_before);
    assert_eq!(status(), status_before);
}
