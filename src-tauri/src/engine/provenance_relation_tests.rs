//! `distance` is kept as measured and the relation to the base is additive.
//! Real git, in temporary repositories.

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
        let mut command = Command::new("git");
        crate::test_support::pin_git(&mut command);
        let out = command
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
}

#[test]
fn a_batch_row_does_not_claim_a_relation_it_did_not_measure() {
    let repo = Repo::new();
    let sha = repo.git(&["rev-parse", "HEAD"]);
    let rows = freshness_batch(repo.as_str(), &[sha], None);
    assert_eq!(rows[0].landed_on_base, None);
    assert!(!rows[0].base_relation_reason.is_empty());
}

/// Every file under an object directory (loose objects, pack files and their
/// indexes) with its bytes, in path order: the listing, the count and the
/// content in one comparable value.
fn object_files(root: &Path) -> Vec<(String, Vec<u8>)> {
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
    let mut out = Vec::new();
    walk(root, root, &mut out);
    out.sort();
    out
}

#[path = "provenance_adversarial_tests.rs"]
mod adversarial;

#[path = "provenance_landing_tests.rs"]
mod landing;

#[path = "provenance_adv_landed_tests.rs"]
mod adv_round2;
