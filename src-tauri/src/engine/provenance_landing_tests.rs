//! `landed` recognises every way a commit reaches the base, and writes nothing
//! while doing it. Real git, in temporary repositories. The `Repo` fixture is
//! the relation tests' own (this module is its child).

use super::*;

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
    let mut store = object_files(&repo.path().join(".git").join("objects"));
    // Every ref too: a textconv cache writes a notes ref, not only objects.
    store.push((
        "<refs: for-each-ref>".to_string(),
        repo.git(&["for-each-ref"]).into_bytes(),
    ));
    store
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

/// A squash with an extra change folded in does not match any patch, but
/// merging the branch into the base is a no-op: the content is there.
fn squash_with_an_unrelated_extra_file() -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    repo.git(&["merge", "--squash", "feature"]);
    std::fs::write(repo.path().join("extra.txt"), "folded in\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "squash feature, plus an extra file"]);
    (repo, tip)
}

#[test]
fn edited_content_that_is_all_on_the_base_reads_tree_and_names_no_commit() {
    let (repo, tip) = squash_with_an_unrelated_extra_file();

    let answer = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.via, Some(LandedVia::Tree), "{answer:?}");
    assert_eq!(answer.merge_commit, None);
    assert_eq!(answer.landed_at, None);
    assert!(
        answer.landed_at_reason.contains("content landing"),
        "{answer:?}"
    );
    assert_eq!(answer.reason, None);
    assert_eq!(answer.error, "");
}

#[test]
fn the_content_comparison_writes_nothing_to_the_repository() {
    // The `tree` route, a conflicting (null) comparison and a clean "not
    // landed" one each run merge-tree, which writes the merged trees.
    #[allow(clippy::type_complexity)]
    let scenarios: [(&str, fn() -> (Repo, String), Option<bool>); 3] = [
        ("tree", squash_with_an_unrelated_extra_file, Some(true)),
        (
            "conflict",
            || {
                let repo = Repo::new();
                repo.git(&["checkout", "-b", "feature"]);
                std::fs::write(repo.path().join("root.txt"), "from feature\n").unwrap();
                repo.git(&["add", "-A"]);
                repo.git(&["commit", "-m", "feature edits root"]);
                let tip = repo.git(&["rev-parse", "HEAD"]);
                repo.git(&["checkout", "main"]);
                std::fs::write(repo.path().join("root.txt"), "from main\n").unwrap();
                repo.git(&["add", "-A"]);
                repo.git(&["commit", "-m", "main edits root"]);
                (repo, tip)
            },
            None,
        ),
        (
            "clean, not landed",
            || {
                let repo = Repo::new();
                repo.git(&["checkout", "-b", "feature"]);
                let tip = repo.commit("feature work", "feature.txt");
                repo.git(&["checkout", "main"]);
                repo.commit("main work", "main.txt");
                (repo, tip)
            },
            Some(false),
        ),
    ];
    for (kind, build, expected) in scenarios {
        let (repo, tip) = build();
        repo.git(&["gc", "--quiet"]);
        let before = object_store(&repo);
        assert!(!before.is_empty());

        let answer = landed(repo.as_str(), &tip, Some("main"));

        assert_eq!(answer.landed, expected, "{kind}: {answer:?}");
        assert!(
            object_store(&repo) == before,
            "{kind}: the object store changed"
        );
    }
}

#[test]
fn a_base_without_a_reflog_of_its_own_is_placed_by_the_commit_date_and_says_so() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "feature"]);
    repo.git(&["tag", "release"]);

    let answer = landed(repo.as_str(), &tip, Some("release"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(answer.landed_at_source, Some(LandedAtSource::CommitDate));
    assert!(answer.landed_at.is_some());
    assert!(answer.landed_at_reason.contains("no reflog"), "{answer:?}");

    // A branch base is placed by its reflog.
    let on_branch = landed(repo.as_str(), &tip, Some("main"));
    assert_eq!(on_branch.landed_at_source, Some(LandedAtSource::Reflog));
    assert_eq!(on_branch.landed_at_reason, "");
}

#[test]
fn an_unresolvable_commit_carries_a_reason_code() {
    let repo = Repo::new();
    let answer = landed(repo.as_str(), "no-such-commit", None);
    assert_eq!(answer.reason, Some(LandedReason::Unresolvable));
    let sha = repo.git(&["rev-parse", "HEAD"]);
    let answer = landed(repo.as_str(), &sha, Some("no-such-branch"));
    assert_eq!(answer.reason, Some(LandedReason::Unresolvable));
}

#[test]
fn a_gate_refusal_is_a_comparison_failure_not_a_landed_false() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.commit("main work", "main.txt");
    let answer = crate::engine::git_cli::with_forced_spawn_failure(|| {
        landed(repo.as_str(), &tip, Some("main"))
    });
    assert_eq!(answer.landed, None, "{answer:?}");
    assert!(answer.reason.is_some(), "{answer:?}");
    assert!(!answer.error.is_empty());
}

#[test]
fn every_git_the_landed_path_spawns_is_told_not_to_lazy_fetch() {
    let repo = Repo::new();
    let env_of = || {
        let run = crate::engine::git_cli::git_captured(
            repo.path(),
            &["-c", "alias.show-env=!env", "show-env"],
        )
        .expect("git");
        String::from_utf8_lossy(&run.stdout).into_owned()
    };
    assert!(
        !env_of().contains("GIT_NO_LAZY_FETCH"),
        "the fence must be scoped to the caller that asks for it"
    );
    let fenced = crate::engine::git_cli::without_lazy_fetch(env_of);
    assert!(fenced.contains("GIT_NO_LAZY_FETCH=1"), "{fenced}");
    // And is lifted again afterwards.
    assert!(!env_of().contains("GIT_NO_LAZY_FETCH"));
}

#[test]
fn a_tag_and_a_branch_of_one_name_are_placed_by_the_ref_resolution_picked() {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "feature"]);
    // A tag and a branch both named `rel`, at different commits. Git's name
    // lookup takes the tag, so the base is the tag's commit and the branch's
    // reflog says nothing about it.
    repo.git(&["tag", "rel"]);
    repo.commit("after the tag", "later.txt");
    repo.git(&["branch", "rel"]);
    assert_ne!(
        repo.git(&["rev-parse", "refs/tags/rel"]),
        repo.git(&["rev-parse", "refs/heads/rel"])
    );

    let answer = landed(repo.as_str(), &tip, Some("rel"));

    assert_eq!(answer.landed, Some(true), "{answer:?}");
    assert_eq!(
        answer.base_oid.as_deref(),
        Some(repo.git(&["rev-parse", "refs/tags/rel"]).as_str())
    );
    assert_eq!(
        answer.landed_at_source,
        Some(LandedAtSource::CommitDate),
        "the tag has no reflog; the branch's must not be read for it: {answer:?}"
    );
    // The unambiguous spellings each use their own.
    let branch = landed(repo.as_str(), &tip, Some("refs/heads/rel"));
    // (the branch was created at a later commit, so its reflog cannot place the
    // landing, and says so in words about the reflog)
    assert_ne!(branch.landed_at_source, Some(LandedAtSource::CommitDate));
    assert!(branch.landed_at_reason.contains("reflog"), "{branch:?}");
    let tag = landed(repo.as_str(), &tip, Some("refs/tags/rel"));
    assert_eq!(tag.landed_at_source, Some(LandedAtSource::CommitDate));
}

/// `count` reverts in a row of one commit on the base: revert, reland, ...
fn reverted_n_times(count: usize) -> (Repo, String) {
    let repo = Repo::new();
    repo.git(&["checkout", "-b", "feature"]);
    let tip = repo.commit("feature work", "feature.txt");
    repo.git(&["checkout", "main"]);
    repo.git(&["merge", "--ff-only", "feature"]);
    let mut current = tip.clone();
    for _ in 0..count {
        repo.git(&["revert", "--no-edit", &current]);
        current = repo.git(&["rev-parse", "HEAD"]);
    }
    (repo, tip)
}

#[test]
fn a_revert_chain_is_read_to_its_end_up_to_the_limit_and_null_beyond_it() {
    for (count, expected) in [(1, Some(true)), (3, Some(true)), (4, Some(false))] {
        let (repo, tip) = reverted_n_times(count);
        let answer = landed(repo.as_str(), &tip, Some("main"));
        assert_eq!(answer.reverted, expected, "{count} reverts: {answer:?}");
    }
    let (repo, tip) = reverted_n_times(5);
    let answer = landed(repo.as_str(), &tip, Some("main"));
    assert_eq!(answer.landed, Some(true));
    assert_eq!(answer.reverted, None, "{answer:?}");
    assert!(
        answer.reverted_reason.starts_with("revert_chain_exceeded"),
        "{answer:?}"
    );
    assert_eq!(answer.reverted_by, None);
}

#[test]
fn absence_is_claimed_only_when_the_base_never_touched_the_branchs_paths() {
    // The base edits a different part of the file the branch edits: merging is
    // clean and adds the branch's lines, but the base did change that path
    // since the fork, so the work may have landed edited and been rewritten.
    let repo = Repo::new();
    std::fs::write(repo.path().join("doc.txt"), "top\nmiddle\nbottom\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "doc"]);
    repo.git(&["checkout", "-b", "feature"]);
    std::fs::write(
        repo.path().join("doc.txt"),
        "top\nmiddle\nbottom\nfeature line\n",
    )
    .unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "feature edits doc"]);
    let tip = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["checkout", "main"]);
    std::fs::write(repo.path().join("doc.txt"), "main top\nmiddle\nbottom\n").unwrap();
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "-m", "main edits doc"]);

    let touched = landed(repo.as_str(), &tip, Some("main"));

    assert_eq!(touched.landed, None, "{touched:?}");
    assert_eq!(touched.reason, Some(LandedReason::ContentNotProvable));

    // The same branch against a base that only changed other files is absent.
    let other = Repo::new();
    other.git(&["checkout", "-b", "feature"]);
    let other_tip = other.commit("feature work", "feature.txt");
    other.git(&["checkout", "main"]);
    other.commit("main work", "main.txt");
    let untouched = landed(other.as_str(), &other_tip, Some("main"));
    assert_eq!(untouched.landed, Some(false), "{untouched:?}");
    assert_eq!(untouched.reason, None);
}
