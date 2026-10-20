//! Adversarial review of the head-divergence cache and of the pairwise merge
//! read: staleness, refusals that must not be cached, repositories that share
//! object names, and an all-unknown answer that must not read as complete.

use super::*;
use crate::engine::git_cli::{spawn_log, with_forced_spawn_failure_of};
use crate::test_support::{git_in, repo_with_worktrees, trust_repo, write};
use std::path::{Path, PathBuf};

fn commit_in(dir: &Path, file: &str, content: &str) -> String {
    write(dir, file, content);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", file]);
    crate::engine::git_cli::git_text(dir, &["rev-parse", "HEAD"])
        .unwrap()
        .trim()
        .to_string()
}

fn clock() -> worktree::ScanDeadline {
    worktree::ScanDeadline::after(Instant::now(), Duration::from_secs(120))
}

fn head_reads(repo: &Path) -> usize {
    spawn_log::spawns_in(repo)
        .iter()
        .filter(|argv| argv.iter().any(|a| a == "--left-right"))
        .count()
}

#[test]
fn a_refused_read_is_unmeasured_and_is_not_remembered_as_an_answer() {
    let (main, _parent, linked) = repo_with_worktrees(1);
    let root = main.path().canonicalize().unwrap();
    let lane = commit_in(&linked[0], "lane.txt", "work\n");

    let refused = with_forced_spawn_failure_of("rev-list", || {
        worktree::measure_head_divergence(&root, &[&lane], &clock())
    });
    assert!(refused.by_head.is_empty(), "{refused:?}");
    assert!(refused
        .unread
        .as_deref()
        .unwrap_or("")
        .starts_with("unmeasured:"));

    let again = worktree::measure_head_divergence(&root, &[&lane], &clock());
    let found = again
        .by_head
        .get(&lane)
        .expect("measured once the gate admits it");
    assert_eq!((found.ahead, found.behind), (1, 0), "{again:?}");
}

#[test]
fn clones_that_share_every_object_name_do_not_share_cache_entries() {
    let (main, _parent, linked) = repo_with_worktrees(1);
    let root = main.path().canonicalize().unwrap();
    let lane = commit_in(&linked[0], "lane.txt", "work\n");
    let copies = tempfile::tempdir().unwrap();
    let copy: PathBuf = copies
        .path()
        .join("copy")
        .canonicalize()
        .unwrap_or_else(|_| copies.path().join("copy"));
    git_in(
        copies.path(),
        &[
            "clone",
            "-q",
            "--no-hardlinks",
            root.to_str().unwrap(),
            copy.to_str().unwrap(),
        ],
    );
    trust_repo(&copy);
    git_in(
        &copy,
        &[
            "fetch",
            "-q",
            root.to_str().unwrap(),
            "--no-tags",
            &format!("{lane}:refs/heads/lane"),
        ],
    );
    let copy = copy.canonicalize().unwrap();

    worktree::measure_head_divergence(&root, &[&lane], &clock());
    let before = head_reads(&copy);
    let measured = worktree::measure_head_divergence(&copy, &[&lane], &clock());

    assert!(measured.by_head.contains_key(&lane), "{measured:?}");
    assert_eq!(
        head_reads(&copy) - before,
        1,
        "the second repository was answered from the first's entry"
    );
}

#[test]
#[ignore = "adv: the cache key (repo, head, base) is held immutable, but a shallow clone deepened in place changes the counts for the same key"]
fn a_deepened_shallow_clone_remeasures_instead_of_serving_the_shallow_counts() {
    let (main, _parent, _linked) = repo_with_worktrees(1);
    let src = main.path().canonicalize().unwrap();
    // fork F, then two commits on the default branch and one on `lane`.
    git_in(&src, &["switch", "-q", "-c", "lane"]);
    let lane = commit_in(&src, "lane.txt", "lane\n");
    git_in(&src, &["switch", "-q", "-"]);
    commit_in(&src, "m1.txt", "1\n");
    commit_in(&src, "m2.txt", "2\n");
    let holder = tempfile::tempdir().unwrap();
    let dst = holder.path().join("shallow");
    let url = format!("file://{}", src.display());
    git_in(
        holder.path(),
        &[
            "clone",
            "-q",
            "--depth",
            "1",
            "--no-single-branch",
            &url,
            dst.to_str().unwrap(),
        ],
    );
    trust_repo(&dst);
    let dst = dst.canonicalize().unwrap();

    let shallow = worktree::measure_head_divergence(&dst, &[&lane], &clock());
    let shallow_counts = shallow.by_head.get(&lane).map(|d| (d.ahead, d.behind));
    git_in(&dst, &["fetch", "-q", "--unshallow"]);
    let deepened = worktree::measure_head_divergence(&dst, &[&lane], &clock());
    let truth = crate::engine::git_cli::git_text(
        &dst,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("HEAD...{lane}"),
        ],
    )
    .unwrap();

    let got = deepened
        .by_head
        .get(&lane)
        .map(|d| format!("{}\t{}", d.behind, d.ahead));
    assert_eq!(
        got.as_deref(),
        Some(truth.trim()),
        "shallow read gave {shallow_counts:?}; after --unshallow the cache still served it"
    );
}

#[test]
#[ignore = "adv: every pair unknown (git merge-tree refused) still reports ok:true, no `deferred`, although ok:true is documented to mean every pair was merged"]
fn an_answer_in_which_no_pair_was_merged_is_not_ok() {
    let (main, _parent, linked) = repo_with_worktrees(2);
    let root = main.path().canonicalize().unwrap();
    commit_in(&linked[0], "a.txt", "a\n");
    commit_in(&linked[1], "b.txt", "b\n");

    let read = with_forced_spawn_failure_of("merge-tree", || {
        merge_conflicts::merge_conflicts(root.to_str().unwrap())
    });

    assert_eq!(read.clean, 0, "{read:?}");
    assert!(read.unknown > 0, "{read:?}");
    assert!(
        !read.ok,
        "ok:true with {} unknown pairs and no error: {read:?}",
        read.unknown
    );
}
