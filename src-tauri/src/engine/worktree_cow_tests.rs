//! What `try_add_worktree_cow_macos` may clone and what git must check out.
//!
//! A clone is only right when the donor's working-tree bytes are exactly what
//! `git checkout` writes for that path in the new worktree. The oracle in
//! these tests is a plain `git worktree add` of the same commit.

use super::*;
use crate::test_support::{git_in, trust_repo};
use std::time::{Duration, SystemTime};

/// Tracked files that conversion cannot change, and so may be cloned.
const CLONEABLE: &[&str] = &[".gitattributes", "plain.txt", "sub/plain2.rs"];
/// Tracked files git must write, because the form checkout produces depends
/// on a filter, an encoding, `ident` or a CRLF conversion.
const CHECKED_OUT: &[&str] = &["run.bat", "doc.filt", "a.id", "w.enc"];

fn old_time() -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000_000)
}

fn set_mtime(path: &Path) {
    std::fs::File::options()
        .write(true)
        .open(path)
        .unwrap()
        .set_modified(old_time())
        .unwrap();
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

fn utf16le(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_le_bytes).collect()
}

/// A GitPulse-shaped donor: `* text=auto eol=lf` plus a CRLF script, a real
/// clean/smudge filter, an `ident` file and a re-encoded file, all checked out
/// by git and then made old so a fresh mtime means "written by this lane".
fn donor(sandbox: &Path) -> PathBuf {
    let main = sandbox.join("main");
    std::fs::create_dir_all(main.join("sub")).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    git_in(&main, &["config", "filter.up.clean", "tr A-Z a-z"]);
    git_in(&main, &["config", "filter.up.smudge", "tr a-z A-Z"]);
    trust_repo(&main);
    std::fs::write(
        main.join(".gitattributes"),
        "* text=auto eol=lf\n*.bat text eol=crlf\n*.filt filter=up\n*.id ident\n\
         *.enc text working-tree-encoding=UTF-16LE eol=lf\n",
    )
    .unwrap();
    std::fs::write(main.join("plain.txt"), "alpha\nbeta\n").unwrap();
    std::fs::write(main.join("sub/plain2.rs"), "fn main() {}\n").unwrap();
    std::fs::write(main.join("run.bat"), "echo hi\n").unwrap();
    std::fs::write(main.join("doc.filt"), "Hello\n").unwrap();
    std::fs::write(main.join("a.id"), "$Id$\n").unwrap();
    std::fs::write(main.join("w.enc"), utf16le("h\u{e9}llo\n")).unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);

    // Make the donor's tree what a checkout writes (filters, CRLF, ident and
    // encoding applied), then age every file.
    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        std::fs::remove_file(main.join(rel)).unwrap();
    }
    git_in(&main, &["checkout", "--", "."]);
    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        set_mtime(&main.join(rel));
    }
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();
    let status = crate::engine::git_cli::git_captured(&main, &["status", "--porcelain"]).unwrap();
    assert!(
        status.stdout.is_empty(),
        "fixture precondition: donor must be clean, got {:?}",
        String::from_utf8_lossy(&status.stdout)
    );
    main
}

#[test]
fn cow_checkout_runs_on_text_auto_eol_lf_and_leaves_converted_paths_to_git() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = donor(sandbox.path());
    let oracle = sandbox.path().join("oracle");
    git_in(
        &main,
        &["worktree", "add", "-b", "oracle", oracle.to_str().unwrap()],
    );
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    for rel in CLONEABLE.iter().chain(CHECKED_OUT) {
        assert_eq!(
            std::fs::read(lane.join(rel)).unwrap(),
            std::fs::read(oracle.join(rel)).unwrap(),
            "{rel} must be byte-identical to a plain `git worktree add`"
        );
    }
    for rel in CLONEABLE {
        assert_eq!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel} was not cloned: its mtime was not preserved"
        );
    }
    for rel in CHECKED_OUT {
        assert_ne!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel} was cloned from the donor but only git may write it"
        );
    }
    let status = crate::engine::git_cli::git_captured(&lane, &["status", "--porcelain"]).unwrap();
    assert!(status.stdout.is_empty(), "lane must be clean");
}

#[test]
fn autocrlf_true_leaves_every_text_path_to_git_and_matches_a_plain_checkout() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = sandbox.path().join("main");
    std::fs::create_dir_all(&main).unwrap();
    git_in(&main, &["init", "-b", "main"]);
    git_in(&main, &["config", "core.autocrlf", "true"]);
    trust_repo(&main);
    std::fs::write(main.join("a.txt"), "one\ntwo\n").unwrap();
    git_in(&main, &["add", "."]);
    git_in(&main, &["commit", "-m", "initial"]);
    std::fs::remove_file(main.join("a.txt")).unwrap();
    git_in(&main, &["checkout", "--", "."]);
    assert_eq!(
        std::fs::read(main.join("a.txt")).unwrap(),
        b"one\r\ntwo\r\n",
        "fixture: the donor's checkout converted to CRLF"
    );
    set_mtime(&main.join("a.txt"));
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();

    let oracle = sandbox.path().join("oracle");
    git_in(
        &main,
        &["worktree", "add", "-b", "oracle", oracle.to_str().unwrap()],
    );
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    assert_eq!(
        std::fs::read(lane.join("a.txt")).unwrap(),
        std::fs::read(oracle.join("a.txt")).unwrap()
    );
    assert_ne!(
        mtime(&lane.join("a.txt")),
        old_time(),
        "a CRLF-converting configuration must not clone the donor's bytes"
    );
}

/// Measurement, not a check. Creates `GITPULSE_COW_MEASURE_N` (default 5)
/// lanes of each arm from the scratch repository named by
/// `GITPULSE_COW_MEASURE_REPO`, interleaved, and prints one line per lane:
/// wall time and the bytes the lane owns (clone-private; a clone of the
/// donor owns nothing). The `plain` arm is `git worktree add`, the path
/// `add_worktree` falls through to when the clone path declines.
#[test]
#[ignore = "measurement: set GITPULSE_COW_MEASURE_REPO to a scratch clone"]
fn cow_lane_creation_cost() {
    let repo = PathBuf::from(std::env::var("GITPULSE_COW_MEASURE_REPO").expect("repo path"));
    let runs: usize = std::env::var("GITPULSE_COW_MEASURE_N")
        .ok()
        .and_then(|n| n.parse().ok())
        .unwrap_or(5);
    trust_repo(&repo);
    let parent = repo.parent().unwrap().to_path_buf();
    for run in 0..runs {
        let arms = if run % 2 == 0 {
            ["plain", "cow"]
        } else {
            ["cow", "plain"]
        };
        for arm in arms {
            let lane = parent.join(format!("lane-{arm}-{run}"));
            let branch = format!("measure/{arm}-{run}");
            let started = std::time::Instant::now();
            if arm == "plain" {
                git_in(
                    &repo,
                    &["worktree", "add", "-b", &branch, lane.to_str().unwrap()],
                );
            } else {
                add_worktree(
                    repo.to_str().unwrap(),
                    lane.to_str().unwrap(),
                    Some(&branch),
                    None,
                    false,
                )
                .expect("add_worktree");
            }
            let wall = started.elapsed();
            // SAFETY: `sync(2)` takes no arguments; it makes freshly written
            // blocks allocated so their private size is not read as zero.
            unsafe { libc::sync() };
            let (private, logical, files, unread) =
                crate::storage::clone_size::private_tree_bytes(&lane);
            println!(
                "MEASURE arm={arm} run={run} wall_ms={} private_bytes={private} logical_bytes={logical} files={files} unread={unread}",
                wall.as_millis()
            );
            git_in(
                &repo,
                &["worktree", "remove", "--force", lane.to_str().unwrap()],
            );
            git_in(&repo, &["branch", "-D", &branch]);
        }
    }
}

fn attrs(
    text: &str,
    eol: &str,
    crlf: &str,
    filter: &str,
    enc: &str,
    ident: &str,
) -> ConversionAttrs {
    ConversionAttrs {
        text: text.into(),
        eol: eol.into(),
        crlf: crlf.into(),
        filter: filter.into(),
        encoding: enc.into(),
        ident: ident.into(),
    }
}

/// A repository with files named so that `git hash-object --stdin-paths`
/// would read a different file than the one named: it unquotes a line that
/// starts with `"` and drops a trailing CR.
fn repo_with_awkward_names(sandbox: &Path, same_content: bool) -> PathBuf {
    let repo = sandbox.join("main");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-b", "main"]);
    trust_repo(&repo);
    let (quoted, cr) = if same_content {
        ("one\n", "three\n")
    } else {
        ("two\n", "four\n")
    };
    std::fs::write(repo.join("x"), "one\n").unwrap();
    std::fs::write(repo.join("\"x\""), quoted).unwrap();
    std::fs::write(repo.join("y"), "three\n").unwrap();
    std::fs::write(repo.join("y\r"), cr).unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-m", "initial"]);
    repo
}

#[test]
fn a_clone_is_never_verified_against_a_file_that_stdin_paths_would_name_instead() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let repo = repo_with_awkward_names(sandbox.path(), false);
    let ls = crate::engine::git_cli::git_captured(&repo, &["ls-files", "-s", "-z"]).unwrap();
    let cloned: Vec<(String, String)> = parse_ls_files_stage0(&ls.stdout)
        .into_iter()
        .map(|(_, oid, rel)| (rel, oid))
        .collect();
    assert_eq!(cloned.len(), 4, "fixture: all four names are tracked");
    // `"x"` and `y\r` are stale; the files `--stdin-paths` would open instead
    // (`x`, `y`) hold exactly the bytes of the blobs those two should have.
    std::fs::write(repo.join("\"x\""), "stale\n").unwrap();
    std::fs::write(repo.join("x"), "two\n").unwrap();
    std::fs::write(repo.join("y\r"), "stale\n").unwrap();
    std::fs::write(repo.join("y"), "four\n").unwrap();

    let wrong = clones_that_are_not_their_blob(&repo, &cloned).unwrap();
    for name in ["\"x\"", "y\r"] {
        assert!(
            wrong.iter().any(|path| path == name),
            "{name:?} is stale and must be reported, got {wrong:?}"
        );
    }
}

#[test]
fn paths_stdin_paths_cannot_name_exactly_are_checked_out_by_git_not_cloned() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let main = repo_with_awkward_names(sandbox.path(), true);
    for rel in ["x", "\"x\"", "y", "y\r"] {
        set_mtime(&main.join(rel));
    }
    let _ = crate::engine::git_cli::git_captured(&main, &["update-index", "--refresh"]).unwrap();
    let lane = sandbox.path().join("lane");
    add_worktree(
        main.to_str().unwrap(),
        lane.to_str().unwrap(),
        Some("lane"),
        None,
        false,
    )
    .expect("add_worktree");

    for rel in ["x", "y"] {
        assert_eq!(mtime(&lane.join(rel)), old_time(), "{rel:?} is cloned");
    }
    for rel in ["\"x\"", "y\r"] {
        assert_ne!(
            mtime(&lane.join(rel)),
            old_time(),
            "{rel:?} cannot be named to `git hash-object --stdin-paths`, so it is not cloned"
        );
        assert_eq!(
            std::fs::read(lane.join(rel)).unwrap(),
            std::fs::read(main.join(rel)).unwrap()
        );
    }
}

#[test]
fn conversion_decision_table() {
    let u = "unspecified";
    // (attributes, default_crlf, converted)
    let table = [
        (attrs(u, u, u, u, u, u), false, false),
        (attrs(u, u, u, u, u, u), true, true),
        (attrs("auto", "lf", u, u, u, u), true, false),
        (attrs("set", "lf", u, u, u, u), false, false),
        (attrs("set", "crlf", u, u, u, u), false, true),
        (attrs("auto", "crlf", u, u, u, u), false, true),
        (attrs("unset", "crlf", u, u, u, u), true, false),
        (attrs("set", u, u, u, u, u), true, true),
        (attrs("set", u, u, u, u, u), false, false),
        (attrs("auto", "weird", u, u, u, u), false, true),
        (attrs(u, "lf", "input", u, u, u), false, true),
        (attrs("unset", u, "unset", u, u, u), false, true),
        (attrs(u, "lf", u, "lfs", u, u), false, true),
        (attrs(u, "lf", u, "unset", u, u), false, false),
        (attrs(u, "lf", u, u, "UTF-16LE", u), false, true),
        (attrs(u, "lf", u, u, u, "set"), false, true),
        (attrs(u, "lf", u, u, u, "unset"), false, false),
    ];
    for (a, default_crlf, expected) in table {
        assert_eq!(
            a.converted_on_checkout(default_crlf),
            expected,
            "{a:?} with default_crlf={default_crlf}"
        );
    }
}

#[test]
fn check_attr_output_must_name_every_attribute_for_every_path() {
    let full = b"a\0text\0auto\0a\0eol\0lf\0a\0crlf\0unspecified\0a\0filter\0unspecified\0\
                 a\0working-tree-encoding\0unspecified\0a\0ident\0unspecified\0";
    let parsed = parse_conversion_attrs(full).unwrap();
    assert_eq!(parsed["a"].text, "auto");
    assert_eq!(parsed["a"].eol, "lf");
    let without_ident = full.strip_suffix(b"a\0ident\0unspecified\0").unwrap();
    assert!(
        parse_conversion_attrs(without_ident).is_err(),
        "a path missing an attribute is an error, not a path with nothing to convert"
    );
    assert!(parse_conversion_attrs(b"a\0text\0auto\0").is_err());
}

#[test]
fn a_clone_whose_bytes_are_not_its_blob_is_reported() {
    let sandbox = tempfile::TempDir::new().unwrap();
    let repo = sandbox.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git_in(&repo, &["init", "-b", "main"]);
    trust_repo(&repo);
    std::fs::write(repo.join("same.txt"), "same\n").unwrap();
    std::fs::write(repo.join("other.txt"), "committed\n").unwrap();
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-m", "initial"]);
    let ls = crate::engine::git_cli::git_captured(&repo, &["ls-files", "-s", "-z"]).unwrap();
    let cloned: Vec<(String, String)> = parse_ls_files_stage0(&ls.stdout)
        .into_iter()
        .map(|(_, oid, rel)| (rel, oid))
        .collect();
    std::fs::write(repo.join("other.txt"), "stale CRLF form\r\n").unwrap();

    assert_eq!(
        clones_that_are_not_their_blob(&repo, &cloned).unwrap(),
        vec!["other.txt".to_string()]
    );
    assert!(clones_that_are_not_their_blob(&repo, &[])
        .unwrap()
        .is_empty());
}
