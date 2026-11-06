use super::*;
use crate::engine::cow_clone::{clone_ignored_caches, CacheSelection};
use crate::test_support::git_in;
use std::fs;
use std::path::PathBuf;

/// A repository whose committed `.gitignore` holds `ignore` (one rule per
/// line), with one linked worktree checked out from that commit. Returns
/// the anchor (canonical) and the worktree.
fn repo(ignore: &str) -> (tempfile::TempDir, tempfile::TempDir, PathBuf, PathBuf) {
    let (main, parent, linked) = crate::test_support::repo_with_worktrees(0);
    crate::test_support::trust_repo(main.path());
    let anchor = main.path().canonicalize().unwrap();
    fs::write(anchor.join(".gitignore"), ignore).unwrap();
    git_in(&anchor, &["add", ".gitignore"]);
    git_in(&anchor, &["commit", "-q", "-m", "ignore rules"]);
    assert!(linked.is_empty());
    let worktree = parent.path().join("wt");
    git_in(
        &anchor,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt",
            worktree.to_str().unwrap(),
        ],
    );
    crate::test_support::trust_repo(&worktree);
    let worktree = worktree.canonicalize().unwrap();
    (main, parent, anchor, worktree)
}

fn write(root: &Path, relative: &str, text: &str) {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

fn sync(anchor: &Path, worktree: &Path) -> CacheReport {
    clone_ignored_caches(anchor, worktree, &CacheSelection::default()).expect("sync ran")
}

#[test]
fn only_files_both_listed_and_ignored_are_copied() {
    let (_main, _parent, anchor, worktree) = repo(".env\n.env.local\nlocal.toml\n");
    write(
        &anchor,
        ".worktreeinclude",
        ".env\nconfig/*.json\nlocal.toml\n",
    );
    write(&anchor, ".env", "SECRET=1");
    // Ignored, not listed.
    write(&anchor, ".env.local", "SECRET=2");
    // Listed, not ignored: an untracked file a copy would show in git status.
    write(&anchor, "config/dev.json", "{}");
    // Listed and ignored.
    write(&anchor, "local.toml", "x = 1");

    let report = sync(&anchor, &worktree);

    assert_eq!(report.failures, Vec::<String>::new());
    let mut included = report.included.clone();
    included.sort();
    assert_eq!(included, [".env", "local.toml"]);
    assert_eq!(
        fs::read_to_string(worktree.join(".env")).unwrap(),
        "SECRET=1"
    );
    assert!(!worktree.join(".env.local").exists());
    assert!(!worktree.join("config/dev.json").exists());
    // The copies are invisible to git in the worktree.
    let status = crate::engine::git_cli::git_text(&worktree, &["status", "--porcelain"]).unwrap();
    assert_eq!(status.trim(), "", "{status}");
}

#[test]
fn a_tracked_file_the_include_names_is_never_copied() {
    let (_main, _parent, anchor, worktree) = repo("*.env\n");
    // Tracked despite the rule (added with -f): it is in the checkout already.
    write(&anchor, "tracked.env", "committed");
    git_in(&anchor, &["add", "-f", "tracked.env"]);
    git_in(&anchor, &["commit", "-q", "-m", "tracked"]);
    fs::write(anchor.join("tracked.env"), "edited in the anchor").unwrap();
    write(&anchor, ".worktreeinclude", "*.env\n");

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty(), "{report:?}");
    assert!(
        !worktree.join("tracked.env").exists(),
        "the worktree branched before it was committed"
    );
}

#[test]
fn a_negated_ignore_rule_is_gits_answer_not_a_copy() {
    let (_main, _parent, anchor, worktree) = repo("*.env\n!keep.env\n");
    write(&anchor, ".worktreeinclude", "*.env\n");
    write(&anchor, "a.env", "a");
    write(&anchor, "keep.env", "not ignored");

    let report = sync(&anchor, &worktree);

    assert_eq!(report.included, ["a.env"]);
    assert!(!worktree.join("keep.env").exists());
}

#[test]
fn a_file_under_an_ignored_directory_is_copied_with_its_parents() {
    let (_main, _parent, anchor, worktree) = repo("secrets/\n");
    write(&anchor, ".worktreeinclude", "secrets/*.pem\n");
    write(&anchor, "secrets/dev.pem", "key");
    write(&anchor, "secrets/notes.txt", "not listed");

    let report = sync(&anchor, &worktree);

    assert_eq!(report.included, ["secrets/dev.pem"]);
    assert_eq!(
        fs::read_to_string(worktree.join("secrets/dev.pem")).unwrap(),
        "key"
    );
    assert!(!worktree.join("secrets/notes.txt").exists());
}

#[test]
fn a_file_only_the_anchor_ignores_is_not_copied() {
    // The rule lives in an uncommitted .gitignore, so the worktree's checkout
    // does not have it and a copy would be an untracked file there.
    let (_main, _parent, anchor, worktree) = repo("");
    write(&anchor, "sub/.gitignore", "local.cfg\n");
    write(&anchor, ".worktreeinclude", "sub/local.cfg\n");
    write(&anchor, "sub/local.cfg", "x");

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty(), "{report:?}");
    assert_eq!(report.failures, Vec::<String>::new());
    assert!(!worktree.join("sub/local.cfg").exists());
}

#[test]
fn an_existing_file_in_the_worktree_is_never_overwritten() {
    let (_main, _parent, anchor, worktree) = repo(".env\n");
    write(&anchor, ".worktreeinclude", ".env\n");
    write(&anchor, ".env", "anchor");
    write(&worktree, ".env", "worktree's own");

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty());
    assert_eq!(
        fs::read_to_string(worktree.join(".env")).unwrap(),
        "worktree's own"
    );
}

#[test]
fn no_include_file_is_no_work_and_no_report() {
    let (_main, _parent, anchor, worktree) = repo(".env\n");
    write(&anchor, ".env", "anchor");

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty());
    assert_eq!(report.failures, Vec::<String>::new());
    assert!(!worktree.join(".env").exists());
}

#[test]
fn cache_directories_are_left_to_the_cache_copy() {
    let (_main, _parent, anchor, worktree) = repo(".venv/\n");
    write(&anchor, ".worktreeinclude", ".venv/\n");
    write(&anchor, ".venv/bin/activate", "VIRTUAL_ENV=/the/anchor");

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty(), "{report:?}");
    assert!(!worktree.join(".venv").exists());
}

#[test]
fn more_files_than_the_cap_copies_none_and_says_why() {
    let (_main, _parent, anchor, worktree) = repo("gen/\n");
    write(&anchor, ".worktreeinclude", "gen/\n");
    for i in 0..=MAX_INCLUDE_FILES {
        write(&anchor, &format!("gen/f{i}.txt"), "x");
    }

    let report = sync(&anchor, &worktree);

    assert!(report.included.is_empty());
    assert!(!worktree.join("gen").exists());
    let error = report.error().expect("the cap is a reported failure");
    assert!(error.contains(".worktreeinclude"), "{error}");
    assert!(error.contains(&MAX_INCLUDE_FILES.to_string()), "{error}");
}

#[cfg(unix)]
#[test]
fn a_link_is_never_followed_on_either_side() {
    let (_main, _parent, anchor, worktree) = repo("*.env\nlinked/\nout/\n");
    write(&anchor, ".worktreeinclude", "*.env\nout/*.env\n");
    let outside = tempfile::tempdir().unwrap();
    write(outside.path(), "real.env", "outside");
    // A link in the anchor: skipped, not copied as its target.
    std::os::unix::fs::symlink(outside.path().join("real.env"), anchor.join("link.env")).unwrap();
    // A listed file whose parent is a link in the worktree: refused.
    write(&anchor, "out/a.env", "a");
    std::os::unix::fs::symlink(outside.path(), worktree.join("out")).unwrap();

    let report = sync(&anchor, &worktree);

    assert!(!worktree.join("link.env").exists());
    assert!(
        !outside.path().join("a.env").exists(),
        "the copy followed a link out of the worktree"
    );
    assert!(
        report.failures.iter().any(|f| f.contains("out/a.env")),
        "{report:?}"
    );
}

#[test]
fn the_app_add_worktree_path_honours_the_include() {
    let (_main, parent, anchor, _worktree) = repo(".env\n");
    write(&anchor, ".worktreeinclude", ".env\n");
    write(&anchor, ".env", "SECRET=1");
    let target = parent.path().join("from-the-app");
    let created = crate::commands::add_worktree_guarded(
        anchor.to_str().unwrap(),
        target.to_str().unwrap(),
        Some("from-the-app"),
        None,
        false,
        true,
        false,
        false,
        None,
        &|repo, argv| crate::harness::guard_command(repo, argv),
    )
    .expect("created");
    assert_eq!(created.output.cache_error, None);
    assert_eq!(fs::read_to_string(target.join(".env")).unwrap(), "SECRET=1");
}
