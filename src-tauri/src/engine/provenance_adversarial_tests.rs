//! Adversarial review of card 15 (S3-11 + S6-06): `landed` honesty, `landed_at`,
//! and the base-relation fields, against real git in temporary repositories.
//!
//! Each test states the answer an honest tool gives. They were written as
//! `#[ignore = "break: ..."]` against the first implementation (every one
//! failed there) and run un-ignored against the rebuilt `landed`.
//! `fixture_matrix` prints what `landed` says for every fixture
//! (`--nocapture`) and always passes.

use super::*;
use crate::procguard::LockedSpawn;
use std::io::Write as _;
use std::process::{Command, Stdio};

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

    /// `git args` with the committer and author date pinned to `ts`.
    fn at(&self, ts: i64, args: &[&str]) -> String {
        let date = format!("@{ts} +0000");
        let out = Command::new("git")
            .args(args)
            .current_dir(self.path())
            .env("GIT_COMMITTER_DATE", &date)
            .env("GIT_AUTHOR_DATE", &date)
            .output_locked()
            .expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// Write `content` to `file` and commit it, returning the sha.
    fn put(&self, file: &str, content: &str, message: &str) -> String {
        std::fs::write(self.path().join(file), content).unwrap();
        self.git(&["add", "-A"]);
        self.git(&["commit", "-m", message]);
        self.git(&["rev-parse", "HEAD"])
    }

    fn land(&self, commit: &str) -> Landed {
        landed(self.as_str(), commit, Some("main"))
    }
}

fn describe(answer: &Landed) -> String {
    format!(
        "landed={:?} via={:?} merge_commit={} error={:?}",
        answer.landed,
        answer.via,
        answer.merge_commit.is_some(),
        answer.error
    )
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

#[test]
fn fixture_matrix() {
    for (name, build) in every_fixture() {
        let (repo, tip) = build();
        let answer = repo.land(&tip);
        println!("MATRIX {name:34} {}", describe(&answer));
    }
}

// --- 1. landed honesty -------------------------------------------------------------

#[test]
fn a_squash_with_conflict_resolution_is_null_with_a_reason_not_a_confident_false() {
    let (repo, tip) = squash_with_conflict_resolution();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, None, "{}", describe(&answer));
    assert_eq!(
        answer.reason,
        Some(LandedReason::ContentNotProvable),
        "{}",
        describe(&answer)
    );
    assert!(!answer.error.is_empty());
}

#[test]
fn a_squash_with_a_fixup_is_null_with_a_reason_not_a_confident_false() {
    let (repo, tip) = squash_with_a_fixup();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, None, "{}", describe(&answer));
    assert_eq!(
        answer.reason,
        Some(LandedReason::ContentNotProvable),
        "{}",
        describe(&answer)
    );
    assert!(!answer.error.is_empty());
}

#[test]
fn a_rebase_with_an_edited_commit_is_null_with_a_reason_not_a_confident_false() {
    let (repo, tip) = rebase_with_an_edited_commit();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, None, "{}", describe(&answer));
    assert_eq!(
        answer.reason,
        Some(LandedReason::ContentNotProvable),
        "{}",
        describe(&answer)
    );
    assert!(!answer.error.is_empty());
}

#[test]
fn a_clean_rebase_is_rebase_and_a_partial_cherry_pick_is_not_landed() {
    let (repo, tip) = rebase_clean();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Rebase));

    let (repo, tip) = partial_cherry_pick();
    let answer = repo.land(&tip);
    // Half the branch is on main. `false` is defensible; it must not be
    // `true`.
    assert_ne!(answer.landed, Some(true), "{}", describe(&answer));
}

/// Reachability cannot see a revert and says landed; so do `git cherry` and
/// the patch-id search. The revert is found by ancestry, from the message `git
/// revert` writes, and reported in `reverted`, beside an unchanged `landed`
/// (the work did reach the base) and `via`.
#[test]
fn a_landing_whose_commit_was_later_reverted_says_so() {
    for (name, build, via) in [
        (
            "ff_then_reverted",
            ff_then_reverted as fn() -> (Repo, String),
            LandedVia::Ff,
        ),
        ("merge_then_reverted", merge_then_reverted, LandedVia::Merge),
        (
            "rebase_then_reverted",
            rebase_then_reverted,
            LandedVia::Squash,
        ),
        (
            "squash_then_reverted",
            squash_then_reverted,
            LandedVia::Squash,
        ),
    ] {
        let (repo, tip) = build();
        let answer = repo.land(&tip);
        assert_eq!(answer.landed, Some(true), "{name}: {}", describe(&answer));
        // A one-commit cherry-pick is the squash route's patch (see
        // `LandedVia::Squash`), so it reads squash, naming the commit.
        assert_eq!(answer.via, Some(via), "{name}: {}", describe(&answer));
        assert_eq!(answer.reverted, Some(true), "{name}: {answer:?}");
        let by = answer.reverted_by.as_deref().expect("reverted_by");
        assert_eq!(
            by,
            repo.git(&["rev-parse", "HEAD"]),
            "{name}: the revert is the tip"
        );
    }
}

/// No revert commit, no `reverted`: a landing that is not reverted says so.
#[test]
fn a_landing_with_no_revert_commit_is_not_reverted() {
    for build in [
        ff_reverted_and_relanded as fn() -> (Repo, String),
        rebase_clean,
        squash_with_a_fixup,
    ] {
        let (repo, tip) = build();
        let answer = repo.land(&tip);
        if answer.landed == Some(true) {
            assert_eq!(answer.reverted, Some(false), "{answer:?}");
            assert_eq!(answer.reverted_by, None);
        }
    }
}

/// `merge -s ours` keeps none of the branch's lines but is a merge commit
/// that joined it: reachable, so landed via merge. Nothing is inferred from
/// content, so it is not `reverted` either (owner ruling: `reverted` only
/// through a revert commit found by ancestry).
#[test]
fn a_strategy_ours_merge_reads_as_a_merge_and_is_not_inferred_reverted() {
    let (repo, tip) = merge_strategy_ours();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Merge));
    assert_eq!(answer.reverted, Some(false), "{answer:?}");
}

#[test]
fn a_relanded_branch_reads_as_landed_via_ff() {
    let (repo, tip) = ff_reverted_and_relanded();
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Ff));
    // The revert was itself reverted: the work is back.
    assert_eq!(answer.reverted, Some(false), "{answer:?}");
}

#[test]
fn the_base_tip_and_an_empty_branch_are_ff_and_say_where_they_landed() {
    for build in [the_base_tip as fn() -> (Repo, String), empty_branch] {
        let (repo, tip) = build();
        let answer = repo.land(&tip);
        assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
        assert_eq!(answer.via, Some(LandedVia::Ff));
        assert_eq!(answer.merge_commit, None);
        assert_eq!(answer.error, "");
    }
}

/// Characterization, not a break: a branch holding only an empty commit, or
/// whose net diff is empty, reads false (it is not on the base, and has no
/// content to find there). Pinned so a change is noticed.
#[test]
fn a_branch_with_nothing_to_land_reads_false() {
    for (name, build) in [
        (
            "only_an_empty_commit",
            only_an_empty_commit as fn() -> (Repo, String),
        ),
        ("net_empty_branch", net_empty_branch),
    ] {
        let (repo, tip) = build();
        let answer = repo.land(&tip);
        assert_eq!(answer.landed, Some(false), "{name}: {}", describe(&answer));
    }
}

/// `git log --max-count=N --reverse` keeps the N *newest* commits, so a squash
/// commit more than N commits below the base tip is never searched.
#[test]
fn a_squash_landing_buried_under_more_than_the_search_window_is_not_a_confident_false() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "work\n", "f adds a");
    let tip = repo.put("b.txt", "more\n", "f adds b");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--squash", "f"]);
    repo.git(&["commit", "-m", "squash f"]);
    // 1,001 later commits on main, by one fast-import.
    let mut stream = String::new();
    for n in 0..=SQUASH_SEARCH_COMMITS {
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@e.com> {} +0000\ndata 1\nx\n{}\n",
            1_700_000_000 + n,
            if n == 0 {
                "from refs/heads/main^0\n"
            } else {
                ""
            }
        ));
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    repo.git(&["reset", "--hard", "main"]);
    println!(
        "BURIED commits on main: {}",
        repo.git(&["rev-list", "--count", "main"])
    );
    let answer = repo.land(&tip);
    println!("BURIED {}", describe(&answer));
    assert_ne!(answer.landed, Some(false), "{}", describe(&answer));
    // Nothing later touched the squashed file, so the merge changes nothing:
    // the content is provably there even though the squash is out of reach.
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Tree), "{}", describe(&answer));
}

/// The same squash, but every later commit edits the squashed file: the patch
/// search cannot reach the squash and merging the branch in conflicts, so
/// absence is not provable and the answer is null with `search_depth_exceeded`.
#[test]
fn a_buried_squash_that_later_commits_edited_is_null_search_depth_exceeded() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "work\n", "f adds a");
    let tip = repo.put("b.txt", "more\n", "f adds b");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--squash", "f"]);
    repo.git(&["commit", "-m", "squash f"]);
    let mut stream = String::new();
    for n in 0..=SQUASH_SEARCH_COMMITS {
        let body = format!("edit {n}\n");
        stream.push_str(&format!(
            "commit refs/heads/main\ncommitter T <t@e.com> {} +0000\ndata 1\nx\n{}M 644 inline a.txt\ndata {}\n{body}\n",
            1_700_000_000 + n,
            if n == 0 { "from refs/heads/main^0\n" } else { "" },
            body.len()
        ));
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    repo.git(&["reset", "--hard", "main"]);
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, None, "{}", describe(&answer));
    assert_eq!(
        answer.reason,
        Some(LandedReason::SearchDepthExceeded),
        "{}",
        describe(&answer)
    );
}

/// `rev-list --first-parent --ancestry-path` is read as "the oldest base
/// first-parent commit descending from the commit"; for a non-tip commit of a
/// `--no-ff`-merged branch that is not the merge.
#[test]
fn a_non_tip_commit_of_a_no_ff_merged_branch_is_merge() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let c1 = repo.put("a.txt", "1\n", "c1");
    repo.put("b.txt", "2\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "m\n", "main moves");
    repo.git(&["merge", "--no-ff", "--no-edit", "f"]);
    let merge = repo.git(&["rev-parse", "HEAD"]);
    let answer = repo.land(&c1);
    assert_eq!(answer.via, Some(LandedVia::Merge), "{}", describe(&answer));
    assert_eq!(answer.merge_commit.as_deref(), Some(merge.as_str()));
}

#[test]
fn via_is_ff_merge_squash_rebase_as_built_and_never_merge_for_a_fast_forward() {
    // ff of a multi-commit branch: asking about the first and the last commit
    // of it, both must be ff.
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let c1 = repo.put("a.txt", "1\n", "c1");
    let c2 = repo.put("b.txt", "2\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "f"]);
    for sha in [&c1, &c2] {
        let answer = repo.land(sha);
        assert_eq!(answer.via, Some(LandedVia::Ff), "{}", describe(&answer));
    }

    // A --no-ff merge: the branch commits are merge, the pre-branch commit on
    // main is ff.
    let repo = Repo::new();
    let before = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "-b", "f"]);
    let c1 = repo.put("a.txt", "1\n", "c1");
    let c2 = repo.put("b.txt", "2\n", "c2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "m\n", "main moves");
    repo.git(&["merge", "--no-ff", "--no-edit", "f"]);
    let _ = c1; // c1 is the break in `a_non_tip_commit_of_a_no_ff_merged_branch_is_merge`
    let answer = repo.land(&c2);
    assert_eq!(
        answer.via,
        Some(LandedVia::Merge),
        "no-ff merge, branch tip: {}",
        describe(&answer)
    );
    assert_eq!(repo.land(&before).via, Some(LandedVia::Ff));

    // An octopus merge.
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "g1"]);
    let g1 = repo.put("g1.txt", "1\n", "g1");
    repo.git(&["checkout", "-b", "g2", "main"]);
    let g2 = repo.put("g2.txt", "2\n", "g2");
    repo.git(&["checkout", "main"]);
    repo.put("m.txt", "m\n", "main moves");
    repo.git(&["merge", "--no-edit", "g1", "g2"]);
    for (label, sha) in [("g1 (first parent side)", &g1), ("g2 (second)", &g2)] {
        let answer = repo.land(sha);
        assert_eq!(
            answer.via,
            Some(LandedVia::Merge),
            "octopus, {label}: {}",
            describe(&answer)
        );
    }

    // Main merged INTO the branch, then the branch fast-forwarded in: the old
    // main tip is reachable through the merge's second parent.
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    repo.put("a.txt", "1\n", "c1");
    repo.git(&["checkout", "main"]);
    let old_main = repo.put("m.txt", "m\n", "main moves");
    repo.git(&["checkout", "f"]);
    repo.git(&["merge", "--no-edit", "main"]);
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "f"]);
    let answer = repo.land(&old_main);
    // Characterization (low): old_main was main's own tip before the
    // fast-forward, yet it reads `merge` with the merge commit named, because
    // after the fast-forward it hangs off the merge's second parent. Git's
    // first-parent history does say so; a reader may not expect it.
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Merge), "{}", describe(&answer));
}

// --- 2. landed_at -----------------------------------------------------------------

/// `main`'s reflog, written whole: each `(commit, unix seconds)` in order.
fn write_reflog(repo: &Repo, branch: &str, entries: &[(&str, i64)]) {
    let zero = "0".repeat(40);
    let mut previous = zero.clone();
    let mut text = String::new();
    for (commit, ts) in entries {
        text.push_str(&format!(
            "{previous} {commit} T <t@e.com> {ts} +0000\tfixture\n"
        ));
        previous = (*commit).to_string();
    }
    let path = repo.path().join(".git/logs/refs/heads").join(branch);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn landed_at_for_the_default_base_head_is_the_landing_not_a_checkout() {
    let repo = Repo::new();
    repo.at(1_000, &["checkout", "-q", "-b", "f"]);
    let c1 = {
        std::fs::write(repo.path().join("a.txt"), "1\n").unwrap();
        repo.git(&["add", "-A"]);
        repo.at(1_200, &["commit", "-m", "c1"]);
        repo.git(&["rev-parse", "HEAD"])
    };
    repo.at(1_300, &["checkout", "-q", "main"]);
    std::fs::write(repo.path().join("m.txt"), "m\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.at(1_400, &["commit", "-m", "main moves"]);
    repo.at(1_600, &["merge", "--no-ff", "--no-edit", "f"]);

    let answer = landed(repo.as_str(), &c1, None);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(
        answer.landed_at,
        Some(1_600),
        "landed_at {:?} ({:?}); the merge onto main was at 1600, the first HEAD \
         reflog entry containing c1 was the commit on f at 1200",
        answer.landed_at,
        answer.landed_at_reason
    );
}

#[test]
fn landed_at_for_a_named_base_is_the_landing() {
    let repo = Repo::new();
    repo.at(1_000, &["checkout", "-q", "-b", "f"]);
    std::fs::write(repo.path().join("a.txt"), "1\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.at(1_200, &["commit", "-m", "c1"]);
    let c1 = repo.git(&["rev-parse", "HEAD"]);
    repo.at(1_300, &["checkout", "-q", "main"]);
    std::fs::write(repo.path().join("m.txt"), "m\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.at(1_400, &["commit", "-m", "main moves"]);
    repo.at(1_600, &["merge", "--no-ff", "--no-edit", "f"]);
    let answer = repo.land(&c1);
    assert_eq!(answer.landed_at, Some(1_600), "{}", describe(&answer));
}

#[test]
fn landed_at_survives_a_reset_and_a_reland() {
    let repo = Repo::new();
    let x0 = repo.git(&["rev-parse", "HEAD"]);
    let landing = repo.put("a.txt", "1\n", "landing");
    let later = repo.put("b.txt", "2\n", "later");
    repo.git(&["reset", "-q", "--hard", &later]);
    write_reflog(
        &repo,
        "main",
        &[
            (&x0, 100),
            (&landing, 200),
            (&x0, 300),
            (&x0, 400),
            (&later, 500),
        ],
    );
    let answer = repo.land(&landing);
    assert_eq!(answer.landed_at, Some(200), "{}", describe(&answer));
}

#[test]
fn landed_at_is_not_claimed_from_an_expired_reflog() {
    let repo = Repo::new();
    let _root = repo.git(&["rev-parse", "HEAD"]);
    let landing = repo.put("a.txt", "1\n", "landing");
    let tip = repo.put("b.txt", "2\n", "later");
    // The landing happened at 1000; the reflog was expired and only the entry
    // from 9000 remains.
    write_reflog(&repo, "main", &[(&tip, 9_000)]);
    let answer = repo.land(&landing);
    assert_eq!(answer.landed, Some(true));
    assert!(
        answer.landed_at != Some(9_000),
        "landed_at says 9000, the only entry left; {}",
        describe(&answer)
    );
}

#[test]
fn an_absent_or_empty_reflog_gives_a_reason_not_a_time() {
    let repo = Repo::new();
    let landing = repo.put("a.txt", "1\n", "landing");
    std::fs::remove_dir_all(repo.path().join(".git/logs")).unwrap();
    let answer = repo.land(&landing);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.landed_at, None);
    assert!(!answer.landed_at_reason.is_empty());
}

/// 1,100 commits and a reflog entry for each, built by one fast-import and a
/// written log: the landing is the commit at `index`.
fn long_reflog(index: usize) -> (Repo, String, Vec<String>) {
    let repo = Repo::new();
    let mut stream = String::new();
    for n in 0..1_100 {
        stream.push_str(
            &format!(
                "commit refs/heads/long\nmark :{}\ncommitter T <t@e.com> {} +0000\ndata 1\nx\n{}\n",
                n + 1,
                1_700_000_000 + n,
                if n == 0 { "" } else { "from :PREV\n" }
            )
            .replace(":PREV", &format!(":{n}")),
        );
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let shas: Vec<String> = repo
        .git(&["rev-list", "--reverse", "long"])
        .lines()
        .map(str::to_string)
        .collect();
    assert_eq!(shas.len(), 1_100);
    repo.git(&["checkout", "-q", "-B", "main", "long"]);
    let entries: Vec<(&str, i64)> = shas
        .iter()
        .enumerate()
        .map(|(n, sha)| (sha.as_str(), 1_700_000_000 + n as i64))
        .collect();
    write_reflog(&repo, "main", &entries);
    let landing = shas[index].clone();
    (repo, landing, shas)
}

#[test]
fn a_reflog_of_more_than_1000_entries_says_lower_bound_for_a_landing_beyond_it() {
    // Older than the 1000 newest entries: no time, with a reason.
    let (repo, old, _) = long_reflog(50);
    let answer = repo.land(&old);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.landed_at, None, "{}", describe(&answer));
    assert!(
        answer.landed_at_reason.contains("1000"),
        "{}",
        answer.landed_at_reason
    );
    // Inside the window: the exact entry's time.
    let (repo, recent, _) = long_reflog(1_050);
    let answer = repo.land(&recent);
    assert_eq!(
        answer.landed_at,
        Some(1_700_000_000 + 1_050),
        "{}",
        describe(&answer)
    );
}

#[test]
fn a_reflog_of_exactly_1000_entries_is_not_called_truncated() {
    let repo = Repo::new();
    let mut stream = String::new();
    for n in 0..1_000 {
        stream.push_str(&format!(
            "commit refs/heads/long\nmark :{}\ncommitter T <t@e.com> {} +0000\ndata 1\nx\n{}\n",
            n + 1,
            1_700_000_000 + n,
            if n == 0 {
                String::new()
            } else {
                format!("from :{n}\n")
            }
        ));
    }
    let mut child = Command::new("git")
        .args(["fast-import", "--quiet"])
        .current_dir(repo.path())
        .stdin(Stdio::piped())
        .spawn_locked()
        .expect("fast-import");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stream.as_bytes())
        .unwrap();
    assert!(child.wait().unwrap().success());
    let shas: Vec<String> = repo
        .git(&["rev-list", "--reverse", "long"])
        .lines()
        .map(str::to_string)
        .collect();
    repo.git(&["checkout", "-q", "-B", "main", "long"]);
    let entries: Vec<(&str, i64)> = shas
        .iter()
        .enumerate()
        .map(|(n, sha)| (sha.as_str(), 1_700_000_000 + n as i64))
        .collect();
    write_reflog(&repo, "main", &entries);
    // The landing is the very first reflog entry: the whole log was read.
    let answer = repo.land(&shas[0]);
    assert_eq!(
        answer.landed_at,
        Some(1_700_000_000),
        "{}",
        describe(&answer)
    );
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
    // Retired: this test pinned `landed == Some(false)` here, the opposite of
    // the ruling that false needs proof. No shared history is no merge to
    // run, and the content may have come in under other ids.
    let answer = repo.land(&island);
    assert_eq!(answer.landed, None, "{}", describe(&answer));
    assert_eq!(
        answer.reason,
        Some(LandedReason::UnrelatedHistory),
        "{}",
        describe(&answer)
    );
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
        let answer = landed(repo.as_str(), &after, Some(tag));
        assert_eq!(answer.landed, Some(false), "{tag}: {}", describe(&answer));
        assert_eq!(
            answer.base_oid.as_deref(),
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
    let answer = landed(repo.as_str(), &tip, None);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
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
    let answer = landed(path, &old, Some("origin/main"));
    assert_ne!(answer.landed, Some(false), "{}", describe(&answer));
    // An unmerged branch tip in a shallow clone: null.
    let answer = landed(path, &feature, Some("origin/main"));
    assert_eq!(answer.landed, None, "{}", describe(&answer));
}

/// A `--filter=blob:none` clone of `origin`, with the commits the fixtures ask
/// about: `(clone dir, [(label, commit)])`. Commits are all present in a
/// partial clone; blobs are what git would fetch.
fn partial_clone_fixture() -> (Repo, tempfile::TempDir, Vec<(&'static str, String)>) {
    let origin = Repo::new();
    origin.git(&["config", "uploadpack.allowFilter", "true"]);
    origin.git(&["config", "uploadpack.allowAnySHA1InWant", "true"]);
    origin.git(&["checkout", "-b", "unmerged"]);
    let unmerged = origin.put("u.txt", "unmerged\n", "unmerged work");
    origin.git(&["checkout", "main"]);
    origin.git(&["checkout", "-b", "squashed"]);
    origin.put("s1.txt", "1\n", "s1");
    let squashed = origin.put("s2.txt", "2\n", "s2");
    origin.git(&["checkout", "main"]);
    let reachable = origin.put("m.txt", "m\n", "main moves");
    origin.git(&["merge", "--squash", "squashed"]);
    origin.git(&["commit", "-m", "squash"]);
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
    assert!(cloned.status.success(), "partial clone failed: {cloned:?}");
    crate::test_support::trust_repo(clone.path());
    (
        origin,
        clone,
        vec![
            ("reachable", reachable),
            ("unmerged", unmerged),
            ("squashed", squashed),
        ],
    )
}

/// The refusal comes before any command that could fetch: the object database
/// of a partial clone is byte-identical (listing and bytes, packs included)
/// after `landed` on every kind of commit, and `landed` says why it did not
/// answer.
#[test]
fn a_partial_clone_is_refused_before_anything_could_fetch_and_nothing_is_written() {
    let (_origin, clone, commits) = partial_clone_fixture();
    let path = clone.path().to_str().unwrap();
    let objects = clone.path().join(".git").join("objects");
    let before = object_files(&objects);
    assert!(!before.is_empty());
    for (label, commit) in &commits {
        for base in [Some("origin/main"), None] {
            let answer = landed(path, commit, base);
            assert_eq!(answer.landed, None, "{label}: {}", describe(&answer));
            assert_eq!(
                answer.reason,
                Some(LandedReason::PartialCloneFetchRequired),
                "{label}: {}",
                describe(&answer)
            );
            assert!(answer.error.contains("partial clone"), "{}", answer.error);
            assert_eq!(answer.via, None);
            assert_eq!(answer.base_oid, None, "nothing was resolved");
        }
        let f = compute_freshness(path, commit, Some("origin/main"));
        assert!(f.landed_on_base.is_some(), "{label}: {f:?}");
        let _ = freshness_batch(path, std::slice::from_ref(commit), Some("origin/main"));
    }
    assert!(
        object_files(&objects) == before,
        "the partial clone's object database changed"
    );
}

/// The refusal is read from the repository's own configuration, not guessed
/// from the clone's shape: a full clone of the same origin answers.
#[test]
fn a_full_clone_of_the_same_origin_is_not_refused() {
    let (origin, _partial, commits) = partial_clone_fixture();
    let clone = tempfile::tempdir().expect("tempdir");
    let url = format!("file://{}", origin.as_str());
    let cloned = Command::new("git")
        .args([
            "clone",
            "--no-single-branch",
            &url,
            clone.path().to_str().unwrap(),
        ])
        .output_locked()
        .expect("git clone");
    assert!(cloned.status.success(), "{cloned:?}");
    crate::test_support::trust_repo(clone.path());
    let path = clone.path().to_str().unwrap();
    for (label, commit) in &commits {
        let answer = landed(path, commit, Some("origin/main"));
        assert_ne!(
            answer.reason,
            Some(LandedReason::PartialCloneFetchRequired),
            "{label}: {}",
            describe(&answer)
        );
        assert!(answer.landed.is_some(), "{label}: {}", describe(&answer));
    }
}

/// A squash merge of a one-commit branch is patch-identical to a cherry-pick
/// of it, and `landed` tries the rebase route first, so the most common squash
/// (a one-commit PR) reports `rebase`, with no `merge_commit` and no
/// `landed_at`, although the squash route would have found both.
#[test]
fn a_one_commit_squash_names_the_squash_commit_and_its_time() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "f"]);
    let tip = repo.put("a.txt", "work\n", "f adds a");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--squash", "f"]);
    repo.git(&["commit", "-m", "squash f"]);
    let squash = repo.git(&["rev-parse", "HEAD"]);
    let answer = repo.land(&tip);
    assert_eq!(answer.landed, Some(true), "{}", describe(&answer));
    assert_eq!(answer.via, Some(LandedVia::Squash), "{}", describe(&answer));
    assert_eq!(answer.merge_commit.as_deref(), Some(squash.as_str()));
    assert!(answer.landed_at.is_some(), "{}", answer.landed_at_reason);
}
