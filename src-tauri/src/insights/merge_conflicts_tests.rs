//! The pairwise merge read against real worktrees: two lanes' committed edits
//! to the same lines conflict (one lane detached), disjoint edits are clean,
//! the object database is untouched, and nothing that did not run reads clean.

use super::*;
use crate::test_support::{git_in, repo_with_worktrees, write};
use std::path::{Path, PathBuf};

/// Commits `file` with `content` in `dir`.
fn commit_file(dir: &Path, file: &str, content: &str) {
    write(dir, file, content);
    git_in(dir, &["add", "-A"]);
    git_in(dir, &["commit", "-q", "-m", file]);
}

/// Every file under `<repo>/.git/objects` with its size and bytes, in path
/// order.
fn object_store(repo: &Path) -> Vec<(String, u64, Vec<u8>)> {
    fn walk(dir: &Path, root: &Path, out: &mut Vec<(String, u64, Vec<u8>)>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, root, out);
            } else {
                let bytes = std::fs::read(&path).unwrap();
                let name = path.strip_prefix(root).unwrap().display().to_string();
                out.push((name, bytes.len() as u64, bytes));
            }
        }
    }
    let root = repo.join(".git").join("objects");
    let mut out = Vec::new();
    walk(&root, &root, &mut out);
    out.sort();
    out
}

fn find<'a>(read: &'a MergeConflicts, a: &Path, b: &Path) -> &'a PairVerdict {
    let is = |path: &str, dir: &Path| crate::insights::same_path(path, dir);
    read.items
        .iter()
        .find(|p| (is(&p.a_path, a) && is(&p.b_path, b)) || (is(&p.a_path, b) && is(&p.b_path, a)))
        .unwrap_or_else(|| panic!("no pair for {a:?} and {b:?} in {read:?}"))
}

fn lanes(count: usize) -> (tempfile::TempDir, tempfile::TempDir, Vec<PathBuf>) {
    let (main, parent, linked) = repo_with_worktrees(count);
    (main, parent, linked)
}

#[test]
fn the_same_lines_edited_in_two_lanes_conflict_and_disjoint_edits_are_clean() {
    let (main, _parent, linked) = lanes(3);
    let root = main.path().canonicalize().unwrap();
    // Lane 0 stays on its branch; lane 1 is detached. Both change the one line
    // of `seed.txt`, differently. Lane 2 adds a file of its own.
    commit_file(&linked[0], "seed.txt", "from lane 0");
    git_in(&linked[1], &["switch", "-q", "--detach"]);
    commit_file(&linked[1], "seed.txt", "from lane 1");
    commit_file(&linked[2], "elsewhere.txt", "lane 2");
    let before = object_store(&root);
    assert!(!before.is_empty());

    let read = merge_conflicts(root.to_str().unwrap());

    assert!(read.ok, "{read:?}");
    assert!(!read.truncated);
    // The main checkout and three lanes: six pairs, all on distinct HEADs
    // except main with nobody (every lane committed).
    assert_eq!(read.worktrees, 4);
    assert_eq!(read.pairs_total, 6, "{read:?}");
    assert_eq!(read.pairs_checked, 6);
    let conflicted = find(&read, &linked[0], &linked[1]);
    assert_eq!(
        conflicted.verdict,
        MergeCandidate::Conflict {
            paths: vec!["seed.txt".to_string()],
            paths_total: 1
        },
        "{conflicted:?}"
    );
    assert!(
        matches!(
            find(&read, &linked[0], &linked[2]).verdict,
            MergeCandidate::Clean { .. }
        ),
        "{read:?}"
    );
    assert!(matches!(
        find(&read, &linked[1], &linked[2]).verdict,
        MergeCandidate::Clean { .. }
    ));
    // Lane 1 is detached and is still compared.
    assert_eq!(read.conflict, 1, "{read:?}");
    assert_eq!(read.clean + read.conflict + read.unknown, 6);
    assert_eq!(read.unknown, 0, "{read:?}");
    assert!(
        object_store(&root) == before,
        "the repository's object store changed"
    );
}

#[test]
fn a_pair_cap_makes_the_answer_a_floor_and_says_so() {
    let (main, _parent, linked) = lanes(3);
    let root = main.path().canonicalize().unwrap();
    for (n, lane) in linked.iter().enumerate() {
        commit_file(lane, &format!("lane{n}.txt"), "work");
    }

    let read = merge_conflicts_within(root.to_str().unwrap(), 2, Duration::from_secs(120));

    assert!(!read.ok, "{read:?}");
    assert!(read.truncated);
    assert_eq!(read.pairs_total, 6);
    assert_eq!(read.pairs_checked, 2);
    assert_eq!(read.items.len(), 2);
    assert!(read.error.contains("cap of 2"), "{}", read.error);
    // The floor never claims more than it merged.
    assert_eq!(read.clean + read.conflict + read.unknown, 2);
}

#[test]
fn a_deadline_that_has_passed_is_unknown_for_every_pair_never_clean() {
    let (main, _parent, linked) = lanes(2);
    let root = main.path().canonicalize().unwrap();
    commit_file(&linked[0], "a.txt", "a");
    commit_file(&linked[1], "b.txt", "b");

    let read = merge_conflicts_within(root.to_str().unwrap(), MAX_MERGE_PAIRS, Duration::ZERO);

    assert_eq!(read.pairs_checked, read.pairs_total, "{read:?}");
    assert!(read.pairs_total > 0);
    assert_eq!((read.clean, read.conflict), (0, 0), "{read:?}");
    assert_eq!(read.unknown, read.pairs_total);
    for pair in &read.items {
        assert!(
            matches!(&pair.verdict, MergeCandidate::Unknown { reason } if reason.contains("deadline")),
            "{pair:?}"
        );
    }
}

#[test]
fn a_git_failure_is_unknown_and_never_clean() {
    let (main, _parent, linked) = lanes(2);
    let root = main.path().canonicalize().unwrap();
    commit_file(&linked[0], "a.txt", "a");
    commit_file(&linked[1], "b.txt", "b");
    let path = root.to_str().unwrap().to_string();
    // The listing and the partial-clone check run, then every spawn is refused.
    let read = crate::engine::git_cli::with_forced_spawn_failure(|| merge_conflicts(&path));
    assert_eq!(read.clean, 0, "{read:?}");
    assert!(!read.ok, "{read:?}");
}

#[test]
fn lanes_on_one_commit_need_no_merge_and_are_counted_apart() {
    let (main, _parent, _linked) = lanes(2);
    let root = main.path().canonicalize().unwrap();

    let read = merge_conflicts(root.to_str().unwrap());

    assert!(read.ok, "{read:?}");
    assert_eq!(read.worktrees, 3);
    assert_eq!(read.pairs_total, 0);
    assert_eq!(read.same_head_pairs, 3);
    assert!(read.items.is_empty());
}

#[test]
fn a_partial_clone_is_refused_once_for_the_whole_call() {
    let (main, _parent, linked) = lanes(2);
    let root = main.path().canonicalize().unwrap();
    commit_file(&linked[0], "a.txt", "a");
    git_in(&root, &["config", "remote.origin.url", "file:///nowhere"]);
    git_in(&root, &["config", "remote.origin.promisor", "true"]);

    let read = merge_conflicts(root.to_str().unwrap());

    assert!(!read.ok);
    assert!(read.error.contains("partial clone"), "{}", read.error);
    assert!(read.items.is_empty());
}

#[test]
fn a_path_that_is_not_a_repository_is_a_failed_read() {
    let dir = tempfile::tempdir().unwrap();
    let read = merge_conflicts(dir.path().to_str().unwrap());
    assert!(!read.ok);
    assert!(!read.error.is_empty());
    assert_eq!(read.pairs_checked, 0);
}
