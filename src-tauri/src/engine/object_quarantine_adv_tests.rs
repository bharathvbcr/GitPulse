//! Adversarial review of `merge_tree_candidate`: inputs built to make a
//! conflicted merge read as clean, or a conflicted path read wrongly, using
//! plumbing so a file name may hold a newline or bytes that are not UTF-8.

use super::*;
use crate::procguard::LockedSpawn;
use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::process::Command;

struct Repo(tempfile::TempDir);

enum Op<'a> {
    Put(&'a str, &'a [u8], &'a [u8]),
    Del(&'a [u8]),
}

impl Repo {
    fn new() -> Self {
        let repo = Repo(tempfile::tempdir().expect("tempdir"));
        repo.git(&["init", "-q", "-b", "main"]);
        repo.git(&["config", "user.name", "T"]);
        repo.git(&["config", "user.email", "t@e.com"]);
        repo.git(&["config", "commit.gpgsign", "false"]);
        crate::test_support::trust_repo(repo.path());
        repo
    }

    fn path(&self) -> &Path {
        self.0.path()
    }

    fn run(&self, args: Vec<OsString>, index: Option<&Path>) -> String {
        let mut cmd = Command::new("git");
        cmd.args(&args).current_dir(self.path());
        if let Some(index) = index {
            cmd.env("GIT_INDEX_FILE", index);
        }
        let out = cmd.output_locked().expect("git");
        assert!(out.status.success(), "git {args:?}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    fn git(&self, args: &[&str]) -> String {
        self.run(args.iter().map(OsString::from).collect(), None)
    }

    fn index(&self) -> PathBuf {
        self.path().join(".git").join("adv-index")
    }

    /// A commit on top of `parent` (or a root) with `ops` applied, built with
    /// plumbing in a private index so no working-tree file is involved.
    fn commit(&self, parent: Option<&str>, ops: &[Op<'_>]) -> String {
        let index = self.index();
        let _ = std::fs::remove_file(&index);
        if let Some(parent) = parent {
            self.run(vec!["read-tree".into(), parent.into()], Some(&index));
        }
        for op in ops {
            match op {
                Op::Put(mode, content, path) => {
                    let oid = crate::test_support::hash_object(self.path(), "blob", content);
                    let mut spec = format!("{mode},{oid},").into_bytes();
                    spec.extend_from_slice(path);
                    self.run(
                        vec![
                            "update-index".into(),
                            "--add".into(),
                            "--cacheinfo".into(),
                            OsString::from_vec(spec),
                        ],
                        Some(&index),
                    );
                }
                Op::Del(path) => {
                    self.run(
                        vec![
                            "update-index".into(),
                            "--force-remove".into(),
                            "--".into(),
                            OsString::from_vec(path.to_vec()),
                        ],
                        Some(&index),
                    );
                }
            }
        }
        self.finish(parent, &index)
    }

    fn finish(&self, parent: Option<&str>, index: &Path) -> String {
        let tree = self.run(vec!["write-tree".into()], Some(index));
        let mut args: Vec<OsString> = vec!["commit-tree".into(), tree.into()];
        if let Some(parent) = parent {
            args.push("-p".into());
            args.push(parent.into());
        }
        args.push("-m".into());
        args.push("m".into());
        self.run(args, None)
    }
}

fn soon() -> Instant {
    Instant::now() + crate::test_support::coverage_relaxed(Duration::from_secs(60))
}

const TEXT: &[u8] = b"one\ntwo\nthree\nfour\nfive\nsix\nseven\neight\nnine\nten\n";

fn conflict_paths(answer: MergeCandidate) -> (Vec<String>, usize) {
    match answer {
        MergeCandidate::Conflict { paths, paths_total } => (paths, paths_total),
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn an_edit_against_a_delete_or_a_rename_is_a_conflict() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"a.txt")]);
    let renamed = repo.commit(
        Some(&base),
        &[Op::Del(b"a.txt"), Op::Put("100644", TEXT, b"b.txt")],
    );
    let deleted = repo.commit(Some(&base), &[Op::Del(b"a.txt")]);
    let edited = repo.commit(Some(&base), &[Op::Put("100644", b"changed\n", b"a.txt")]);

    // An edit follows a rename: git applies it to the new name, cleanly.
    let followed = merge_tree_candidate(repo.path(), &edited, &renamed, soon());
    assert!(
        matches!(followed, MergeCandidate::Clean { .. }),
        "{followed:?}"
    );
    // Rename on one side, delete on the other, is a rename/delete conflict.
    let (paths, _) = conflict_paths(merge_tree_candidate(
        repo.path(),
        &deleted,
        &renamed,
        soon(),
    ));
    assert!(!paths.is_empty());
    let (paths, _) = conflict_paths(merge_tree_candidate(repo.path(), &edited, &deleted, soon()));
    assert_eq!(paths, vec!["a.txt"]);
}

#[test]
fn a_file_turned_into_a_symlink_against_an_edit_of_it_is_a_conflict() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"f")]);
    let link = repo.commit(Some(&base), &[Op::Put("120000", b"elsewhere", b"f")]);
    let edit = repo.commit(Some(&base), &[Op::Put("100644", b"edited\n", b"f")]);
    // git names the path and the `f~<oid>` side it moved the other version to.
    let (paths, _) = conflict_paths(merge_tree_candidate(repo.path(), &link, &edit, soon()));
    assert!(paths.iter().any(|p| p == "f"), "{paths:?}");
}

#[test]
fn two_different_modes_and_contents_is_not_clean() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"f")]);
    let a = repo.commit(Some(&base), &[Op::Put("100755", b"a\n", b"f")]);
    let b = repo.commit(Some(&base), &[Op::Put("100644", b"b\n", b"f")]);
    let (paths, _) = conflict_paths(merge_tree_candidate(repo.path(), &a, &b, soon()));
    assert_eq!(paths, vec!["f"]);
}

#[test]
fn two_different_gitlink_commits_are_not_clean() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"f")]);
    let mk = |oid: String| {
        let index = repo.index();
        let _ = std::fs::remove_file(&index);
        repo.run(vec!["read-tree".into(), base.clone().into()], Some(&index));
        repo.run(
            vec![
                "update-index".into(),
                "--add".into(),
                "--cacheinfo".into(),
                format!("160000,{oid},sub").into(),
            ],
            Some(&index),
        );
        repo.finish(Some(&base), &index)
    };
    let a = mk("1".repeat(40));
    let b = mk("2".repeat(40));
    let answer = merge_tree_candidate(repo.path(), &a, &b, soon());
    assert!(
        !matches!(answer, MergeCandidate::Clean { .. }),
        "two different submodule commits merged clean: {answer:?}"
    );
}

#[test]
fn two_binary_edits_are_a_conflict() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", b"\0\x01\x02base", b"bin")]);
    let a = repo.commit(Some(&base), &[Op::Put("100644", b"\0\x01\x02a", b"bin")]);
    let b = repo.commit(Some(&base), &[Op::Put("100644", b"\0\x01\x02b", b"bin")]);
    let (paths, _) = conflict_paths(merge_tree_candidate(repo.path(), &a, &b, soon()));
    assert_eq!(paths, vec!["bin"]);
}

#[test]
fn a_criss_cross_with_conflicting_edits_is_not_clean() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"f")]);
    let a1 = repo.commit(Some(&base), &[Op::Put("100644", b"a1\n", b"x")]);
    let b1 = repo.commit(Some(&base), &[Op::Put("100644", b"b1\n", b"y")]);
    // Each side merges the other: two merge bases (a1 and b1).
    let merge = |p1: &str, p2: &str| {
        let tree = repo.run(
            vec![
                "merge-tree".into(),
                "--write-tree".into(),
                p1.into(),
                p2.into(),
            ],
            None,
        );
        repo.run(
            vec![
                "commit-tree".into(),
                tree.into(),
                "-p".into(),
                p1.into(),
                "-p".into(),
                p2.into(),
                "-m".into(),
                "x".into(),
            ],
            None,
        )
    };
    let am = merge(&a1, &b1);
    let bm = merge(&b1, &a1);
    let a2 = repo.commit(Some(&am), &[Op::Put("100644", b"left\n", b"f")]);
    let b2 = repo.commit(Some(&bm), &[Op::Put("100644", b"right\n", b"f")]);
    let (paths, _) = conflict_paths(merge_tree_candidate(repo.path(), &a2, &b2, soon()));
    assert_eq!(paths, vec!["f"]);
}

#[test]
fn a_path_with_a_newline_is_one_path() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"a\nb.txt")]);
    let a = repo.commit(Some(&base), &[Op::Put("100644", b"a\n", b"a\nb.txt")]);
    let b = repo.commit(Some(&base), &[Op::Put("100644", b"b\n", b"a\nb.txt")]);
    let (paths, total) = conflict_paths(merge_tree_candidate(repo.path(), &a, &b, soon()));
    assert_eq!((paths, total), (vec!["a\nb.txt".to_string()], 1));
}

#[test]
fn two_conflicted_paths_that_are_not_utf8_stay_distinguishable() {
    let repo = Repo::new();
    let base = repo.commit(
        None,
        &[
            Op::Put("100644", TEXT, b"\xff.txt"),
            Op::Put("100644", TEXT, b"\xfe.txt"),
        ],
    );
    let a = repo.commit(
        Some(&base),
        &[
            Op::Put("100644", b"a\n", b"\xff.txt"),
            Op::Put("100644", b"a\n", b"\xfe.txt"),
        ],
    );
    let b = repo.commit(
        Some(&base),
        &[
            Op::Put("100644", b"b\n", b"\xff.txt"),
            Op::Put("100644", b"b\n", b"\xfe.txt"),
        ],
    );
    let (paths, total) = conflict_paths(merge_tree_candidate(repo.path(), &a, &b, soon()));
    assert_eq!(total, 2);
    let distinct: std::collections::BTreeSet<&String> = paths.iter().collect();
    assert_eq!(
        distinct.len(),
        2,
        "two different paths were reported as {paths:?}"
    );
}

#[test]
fn more_than_the_cap_of_conflicted_paths_keeps_the_true_total() {
    let repo = Repo::new();
    let names: Vec<String> = (0..250).map(|n| format!("f{n:03}")).collect();
    let put = |content: &'static [u8]| -> Vec<Op<'_>> {
        names
            .iter()
            .map(|n| Op::Put("100644", content, n.as_bytes()))
            .collect()
    };
    let base = repo.commit(None, &put(TEXT));
    let a = repo.commit(Some(&base), &put(b"a\n"));
    let b = repo.commit(Some(&base), &put(b"b\n"));
    let (paths, total) = conflict_paths(merge_tree_candidate(repo.path(), &a, &b, soon()));
    assert_eq!((paths.len(), total), (MAX_CONFLICT_PATHS, 250));
}

#[test]
fn a_directory_against_a_file_of_the_same_name_is_not_clean() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"keep")]);
    let file = repo.commit(Some(&base), &[Op::Put("100644", b"file\n", b"d")]);
    let dir = repo.commit(Some(&base), &[Op::Put("100644", b"inside\n", b"d/x")]);
    let (paths, total) = conflict_paths(merge_tree_candidate(repo.path(), &file, &dir, soon()));
    assert!(
        paths.iter().all(|p| !p.is_empty()) && total == paths.len(),
        "{paths:?}"
    );
}

#[test]
fn a_quarantined_merge_leaves_the_whole_git_dir_and_the_refs_as_found() {
    let repo = Repo::new();
    let base = repo.commit(None, &[Op::Put("100644", TEXT, b"f")]);
    let a = repo.commit(Some(&base), &[Op::Put("100644", b"a\n", b"f")]);
    let b = repo.commit(Some(&base), &[Op::Put("100644", b"b\n", b"f")]);
    let c = repo.commit(Some(&base), &[Op::Put("100644", b"c\n", b"g")]);
    repo.git(&["update-ref", "refs/heads/keep", &a]);
    let _ = std::fs::remove_file(repo.index());
    let snapshot = |repo: &Repo| {
        let mut out = Vec::new();
        let root = repo.path().join(".git");
        let mut stack = vec![root.clone()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push((
                        path.strip_prefix(&root).unwrap().display().to_string(),
                        std::fs::read(&path).unwrap(),
                    ));
                }
            }
        }
        out.sort();
        out
    };
    let before = snapshot(&repo);
    assert!(matches!(
        merge_tree_candidate(repo.path(), &a, &b, soon()),
        MergeCandidate::Conflict { .. }
    ));
    assert!(matches!(
        merge_tree_candidate(repo.path(), &a, &c, soon()),
        MergeCandidate::Clean { .. }
    ));
    assert!(snapshot(&repo) == before, "the git directory changed");
}
